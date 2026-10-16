// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The unix (Linux + macOS) control endpoint: a per-user stream socket.
//!
//! The daemon binds `$XDG_RUNTIME_DIR/keymapperd.sock` (or a per-user cache
//! directory when the runtime dir is unset) and serves one command per
//! connection. The socket is the auth mechanism, so it is created `0600`
//! and owned by the daemon's uid — the current user — meaning only that user
//! can connect. The bind runs under a temporarily tightened `umask` so the
//! socket is born owner-only, with no `bind`-to-`chmod` window; as defense
//! in depth every accepted connection is additionally checked against the
//! peer's kernel-reported credentials (see [`authorize_peer`]).
//!
//! The cache-directory fallback may live below a world-writable base such
//! as `/tmp`, where a local attacker could plant the endpoint directory and
//! later unlink or replace the socket to impersonate the daemon to the CLI.
//! The daemon therefore refuses to bind unless the socket's parent
//! directory is owned by the current user, and creates it `0700` when it is
//! missing (see [`prepare_socket_dir`]).
//!
//! The transport runs a daemon-supplied [`ConnectionHandler`] once per
//! accepted, authorized connection; it never names the protocol above it.

use std::{
    fs::Permissions,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};

use log::{debug, info, warn};

use super::{ConnectionHandler, Endpoint, IoStream, RateLimiter};

/// Peer-credential lookup, per OS: `SO_PEERCRED` on Linux, `getpeereid(2)`
/// on macOS. Kept in separate files because the socket option and its struct
/// differ per OS.
#[cfg(target_os = "macos")]
#[path = "peer_uid_macos.rs"]
mod peer_uid;
#[cfg(not(target_os = "macos"))]
#[path = "peer_uid_linux.rs"]
mod peer_uid;

/// The application name, used in the fallback endpoint directory.
const APP_NAME: &str = "keymapperd";

/// The control socket file name.
const SOCKET_NAME: &str = "keymapperd.sock";

/// Read and write timeout for an accepted control connection.
///
/// The server handles one connection at a time, so a peer that connects
/// and stalls (sending nothing, or never reading the reply) would
/// otherwise block the accept loop until it exits. The timeout turns a
/// stuck peer into a closed connection; a well-behaved local CLI
/// completes the exchange in milliseconds.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// Read and write timeout for a *client* control connection.
///
/// The server-side [`IO_TIMEOUT`] is generous (10 s) so a stalled peer can
/// drain its reply; the client side is far stricter (1 s) so a dead or
/// impersonating daemon surfaces as an error instead of hanging the CLI
/// (R3-4). A normal local round-trip completes in milliseconds.
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(1);

/// Sliding-window size for per-uid connection rate limiting (Finding 6).
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(1);

/// Maximum accepted connections per *uid* within [`RATE_LIMIT_WINDOW`].
const RATE_LIMIT_THRESHOLD: usize = 10;

/// The unix control endpoint.
pub(crate) struct UnixEndpoint;

/// The control endpoint path.
///
/// `$XDG_RUNTIME_DIR/keymapperd.sock` when the runtime dir is set and
/// absolute (the normal case under a per-user systemd session); otherwise a
/// per-user cache directory. Both the daemon and the CLI resolve this the same
/// way from the same environment, so they always agree.
///
/// A relative or empty `XDG_RUNTIME_DIR` is rejected — it would silently
/// re-root the socket into the CWD, where an attacker could plant a listening
/// socket and impersonate the daemon to the CLI.
fn socket_path() -> PathBuf {
    if let Some(rd) = std::env::var_os("XDG_RUNTIME_DIR") {
        let rd = PathBuf::from(rd);
        if !rd.as_os_str().is_empty() && rd.is_absolute() {
            return rd.join(SOCKET_NAME);
        }
    }
    let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    base.join(APP_NAME).join(SOCKET_NAME)
}

/// Validate (and if necessary create) the socket's parent directory.
///
/// Deletion permission on a unix socket comes from the *containing
/// directory*, so whoever owns the parent can unlink or replace the socket
/// and impersonate the daemon to the CLI. A bind is therefore only allowed
/// when *parent* is a directory owned by *current_uid*.
///
/// As root the check is relaxed to "not world-writable": a root-run daemon
/// may legitimately sit in a user-owned runtime directory, so ownership
/// proves nothing there. This mirrors the policy of the hardened config
/// reader (`config_io`).
///
/// A directory that does not exist yet is created with mode `0700`
/// (`create_dir_all` would apply the process umask); a pre-existing one —
/// e.g. a systemd `XDG_RUNTIME_DIR` — is left as it is.
fn prepare_socket_dir(parent: &Path, current_uid: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let mut created = false;
    let meta = match fs_err::metadata(parent) {
        Ok(meta) => meta,
        Err(_) => {
            fs_err::create_dir_all(parent).map_err(|e| {
                format!("cannot create {}: {e}", parent.display())
            })?;
            created = true;
            fs_err::metadata(parent).map_err(|e| {
                format!("cannot stat {}: {e}", parent.display())
            })?
        }
    };
    if !meta.is_dir() {
        return Err(format!("{} is not a directory", parent.display()));
    }
    if !created {
        if current_uid == 0 {
            if meta.mode() & 0o002 != 0 {
                return Err(format!(
                    "refusing to bind in {}: directory is world-writable",
                    parent.display(),
                ));
            }
        } else if meta.uid() != current_uid {
            return Err(format!(
                "refusing to bind in {}: directory is owned by uid {}, not \
                 by the current uid {current_uid}",
                parent.display(),
                meta.uid(),
            ));
        }
    }
    if created {
        fs_err::set_permissions(parent, Permissions::from_mode(0o700))
            .map_err(|e| {
                format!("cannot set permissions on {}: {e}", parent.display())
            })?;
    }
    Ok(())
}

/// Bind the endpoint at *path*, removing any stale socket first.
///
/// The parent directory is validated (and created, if missing) by
/// [`prepare_socket_dir`] before the stale-socket unlink and the bind, so the
/// unlink below can only ever remove an entry in a directory that belongs to
/// us.
///
/// The bind runs under a temporarily tightened umask so the socket inode is
/// created owner-only (`0777 & ~0o177` = `0600`) *at creation time*. A
/// `chmod` after the bind would leave a window — the socket is born
/// `0777 & ~umask`, so under a permissive umask another local user could
/// connect before the mode is corrected.
fn bind_at(path: &Path) -> Result<UnixListener, String> {
    if let Some(parent) = path.parent() {
        prepare_socket_dir(parent, unsafe { libc::getuid() })?;
    }
    // Remove a stale socket left by a previous (crashed) instance; a fresh
    // bind otherwise fails with `AddressInUse`.
    let _ = fs_err::remove_file(path);

    // `umask` is process-global, so this briefly affects file creations by
    // other threads. The deviation is microsecond-sized and one-directional:
    // other threads' files can only end up *more* restrictive, never more
    // permissive.
    let previous_umask = unsafe { libc::umask(0o177) };
    let bound = UnixListener::bind(path);
    unsafe { libc::umask(previous_umask) };
    // Owner-only from the first instant: the socket is the auth mechanism,
    // so it must never exist in a group- or world-writable state.
    bound.map_err(|e| e.to_string())
}

/// Accept connections and serve one command per connection, applying the
/// default [`IO_TIMEOUT`] to each accepted stream.
fn serve(listener: UnixListener, handler: ConnectionHandler) {
    serve_with_timeout(listener, IO_TIMEOUT, handler);
}

/// Accept connections and serve one command per connection, applying
/// *io_timeout* to each accepted stream.
fn serve_with_timeout(
    listener: UnixListener,
    io_timeout: Duration,
    handler: ConnectionHandler,
) {
    let mut rate_limiter =
        RateLimiter::new(RATE_LIMIT_WINDOW, RATE_LIMIT_THRESHOLD);
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        // Authoritative peer check, mirroring the `getpeereid` gate in the
        // virtkbdd IPC server: the file mode is the first gate, but kernel
        // credentials are the source of truth, so a permission race (or a
        // mode weakened by an external actor) still cannot admit a foreign
        // user.  The peer uid is threaded into the handler so the protocol
        // layer can enforce uid-dependent policy (e.g. refusing root log-
        // level escalation on a user-run daemon).
        let peer_uid = match authorize_peer(&stream, unsafe { libc::getuid() })
        {
            Ok(uid) => uid,
            Err(e) => {
                debug!("control-socket rejecting connection: {e}");
                continue;
            }
        };
        // Rate limit: reject excess connections per uid so a single peer
        // cannot flood the single-threaded accept loop (Finding 6).
        if !rate_limiter.check_and_record(peer_uid) {
            debug!("control-socket rate limit exceeded for uid {peer_uid}");
            continue;
        }
        // Bound the I/O before touching the stream so a stalled peer
        // cannot wedge this single-threaded loop; a timeout surfaces as
        // an I/O error and closes the connection.
        let timeout = Some(io_timeout);
        if stream.set_read_timeout(timeout).is_err()
            || stream.set_write_timeout(timeout).is_err()
        {
            continue;
        }
        // The handler owns the protocol and its own connection logging; the
        // transport only recycles the stream (a fresh one per connection)
        // afterward.
        handler(&mut stream, peer_uid);
    }
}

/// Verify that *stream* comes from a process allowed to use the control
/// endpoint.
///
/// The socket is owner-only (`0600`), so file permissions alone should admit
/// only the daemon's own uid. Peer credentials are checked anyway because
/// they are authoritative: the kernel reports them (`SO_PEERCRED` on Linux,
/// `LOCAL_PEERCRED` on macOS) and they cannot be forged, while file modes
/// can be raced or changed behind the daemon's back.
///
/// Root is admitted as well: it bypasses file-permission checks anyway, so
/// refusing it would not confine root but would break `sudo keymapper ...`
/// against a user-run daemon.
fn authorize_peer(
    stream: &UnixStream,
    current_uid: libc::uid_t,
) -> Result<u32, String> {
    let peer = peer_uid::peer_uid(stream)?;
    if peer == 0 || peer == current_uid {
        Ok(peer)
    } else {
        Err(format!(
            "peer uid {peer} is neither the daemon uid nor root"
        ))
    }
}

/// Connect to the endpoint at *path* as a client.
fn connect_at(path: &Path) -> std::io::Result<UnixStream> {
    UnixStream::connect(path)
}

/// Verify the server's uid after connecting as a client (R3-4).
///
/// The daemon runs as root (uid 0) or as the current user; both are
/// accepted. Any other uid means an attacker has squatted the socket path in
/// a writable directory, so the connection is dropped before any command is
/// exchanged. *current_uid* is passed explicitly to keep the check testable
/// without real uid switching, mirroring [`authorize_peer`].
fn verify_server_uid(
    stream: &UnixStream,
    current_uid: libc::uid_t,
) -> std::io::Result<()> {
    let peer = peer_uid::peer_uid(stream).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, e)
    })?;
    if peer == 0 || peer == current_uid {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "control endpoint peer uid {peer} is neither root nor the \
                 current uid {current_uid}; refusing to talk to a non-daemon"
            ),
        ))
    }
}

impl Endpoint for UnixEndpoint {
    /// Bind the endpoint and spawn the accept thread.
    fn start(handler: ConnectionHandler) {
        let path = socket_path();
        let listener = match bind_at(&path) {
            Ok(listener) => listener,
            Err(e) => {
                // The control socket is a convenience; a bind failure must not
                // stop the daemon. The level then stays at the env-var seed.
                warn!(
                    "Control socket unavailable ({e}); runtime log-level \
                     control is disabled"
                );
                return;
            }
        };
        info!("Control socket listening on {:?}", path);

        if let Err(e) = std::thread::Builder::new()
            .name("control-socket".into())
            .spawn(move || serve(listener, handler))
        {
            warn!("Failed to spawn the control-socket thread: {e}");
        }
    }

    /// Connect to the daemon's control endpoint as a client.
    ///
    /// After connecting, the server's uid is verified (R3-4): only root or
    /// the current user's daemon is accepted, so an attacker who squats the
    /// socket path in a writable directory cannot impersonate the daemon to
    /// the CLI. A 1 s read/write timeout ([`CLIENT_IO_TIMEOUT`]) is also set
    /// so a non-responsive endpoint surfaces as an error instead of hanging
    /// the CLI.
    fn connect() -> std::io::Result<Box<dyn IoStream>> {
        let stream = connect_at(&socket_path())?;
        verify_server_uid(&stream, unsafe { libc::getuid() })?;
        stream.set_read_timeout(Some(CLIENT_IO_TIMEOUT))?;
        stream.set_write_timeout(Some(CLIENT_IO_TIMEOUT))?;
        Ok(Box::new(stream))
    }

    /// Convert a failed client connection into a message the CLI can turn
    /// into a clear "daemon not running / older than CLI" hint.
    fn connect_error(e: &std::io::Error) -> String {
        match e.kind() {
            std::io::ErrorKind::NotFound => "no daemon is running (or its \
                                             control socket is not at the \
                                             expected path)"
                .to_string(),
            std::io::ErrorKind::ConnectionRefused => {
                "no daemon control socket is listening (the daemon may be \
                 older than this CLI)"
                    .to_string()
            }
            _ => e.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::common::frame;

    /// The payload bound the round-trip tests frame with (mirrors the
    /// protocol's `MAX_PAYLOAD`).
    const MAX: usize = 256;

    /// A serve-loop handler that echoes one request frame back as the reply.
    /// It keeps the transport tests independent of the daemon protocol while
    /// still exercising a full request/response round trip over the socket.
    fn echo(io: &mut dyn IoStream, _peer_uid: u32) -> bool {
        let Ok(command) = frame::read_payload(io, MAX) else {
            return false;
        };
        frame::write_frame(io, &command).is_ok()
    }

    /// Connect with a short retry, so a freshly bound (but not yet accepted)
    /// listener is reachable.
    fn wait_for_socket(
        path: &Path,
        timeout: Duration,
    ) -> std::io::Result<UnixStream> {
        let start = std::time::Instant::now();
        loop {
            match UnixStream::connect(path) {
                Ok(s) => return Ok(s),
                Err(_) if start.elapsed() < timeout => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// The transport end to end over a real unix socket: a `serve` thread, a
    /// client that connects and round-trips a frame. This exercises binding,
    /// the accept loop, the owner-only auth gate, and the framed read/write;
    /// the command dispatch on top of it is covered by `daemon::control`.
    #[test]
    fn control_socket_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SOCKET_NAME);

        let listener = match bind_at(&path) {
            Ok(listener) => listener,
            Err(e) => {
                // Some sandboxes forbid unix domain sockets (EPERM); skip the
                // live round trip there. The auth and directory checks are
                // covered by the other tests; the socket plumbing is verified
                // in CI / on a host that allows the bind.
                eprintln!(
                    "skipping control_socket_round_trip: cannot bind ({e})"
                );
                return;
            }
        };
        // Detached: `serve` blocks on accept for the process lifetime, which
        // is fine for a test socket in a temp dir.
        std::thread::spawn(move || serve(listener, echo));

        let mut stream =
            wait_for_socket(&path, Duration::from_secs(2)).unwrap();
        // A timeout turns a stuck (e.g. unimplemented) server into an error
        // instead of a hung test.
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        let command = b"SET-LOG-LEVEL debug";
        frame::write_frame(&mut stream, command).unwrap();
        let reply = frame::read_payload(&mut stream, MAX).unwrap();
        assert_eq!(reply, command);
    }

    /// A peer that connects and sends nothing must not wedge the
    /// single-threaded accept loop: once its read times out the server
    /// closes the connection and serves the next client.
    #[test]
    fn idle_peer_does_not_wedge_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SOCKET_NAME);

        let listener = match bind_at(&path) {
            Ok(listener) => listener,
            Err(e) => {
                eprintln!(
                    "skipping idle_peer_does_not_wedge_the_server: cannot \
                     bind ({e})"
                );
                return;
            }
        };
        // A short timeout so the recycling is observed quickly.
        std::thread::spawn(move || {
            serve_with_timeout(listener, Duration::from_millis(200), echo)
        });

        // A silent peer occupies the loop until its read times out.
        let _idle = wait_for_socket(&path, Duration::from_secs(2)).unwrap();

        // A second client is served once the first has been dropped.
        let mut stream =
            wait_for_socket(&path, Duration::from_secs(2)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let command = b"SET-LOG-LEVEL debug";
        frame::write_frame(&mut stream, command).unwrap();
        let reply = frame::read_payload(&mut stream, MAX).unwrap();
        assert_eq!(reply, command);
    }

    /// The socket must be born owner-only even under a fully permissive
    /// umask: the bind runs under `umask(0o177)`, so there is no window in
    /// which another user could connect (SEC-11). Mutating the umask is
    /// process-global and therefore only safe because nextest runs each test
    /// in its own process.
    #[test]
    fn bind_at_creates_owner_only_socket_under_permissive_umask() {
        use std::os::unix::fs::MetadataExt;

        unsafe { libc::umask(0o000) };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SOCKET_NAME);
        let listener = match bind_at(&path) {
            Ok(listener) => listener,
            Err(e) => {
                eprintln!(
                    "skipping owner-only-socket bind test: cannot bind ({e})"
                );
                return;
            }
        };
        // Without the umask scope the socket would be created 0777 here; the
        // mode is read while the listener is alive, i.e. the moment an
        // attacker could still connect.
        let mode = fs_err::metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
        drop(listener);
    }

    /// `authorize_peer` admits the daemon's own uid (and root) and rejects
    /// any other uid. A real foreign-uid peer cannot be created without
    /// privileges, so the rejection path is exercised through the uid
    /// parameter, mirroring `socket_dir_rejects_foreign_owner`.
    #[test]
    fn authorize_peer_checks_peer_credentials() {
        let (server, _client) = UnixStream::pair().unwrap();

        // The peer of a socketpair is this very process, i.e. the current
        // uid.
        authorize_peer(&server, unsafe { libc::getuid() }).unwrap();

        let foreign_uid = unsafe { libc::getuid() } + 1;
        let err = authorize_peer(&server, foreign_uid).unwrap_err();
        assert!(err.contains("neither the daemon uid nor root"));
    }

    /// A parent directory owned by a different uid must be refused: its
    /// owner could unlink or replace the socket. A real foreign-owned
    /// directory cannot be created without privileges, so the check itself
    /// is exercised through the uid parameter.
    #[test]
    fn socket_dir_rejects_foreign_owner() {
        let dir = tempfile::tempdir().unwrap();
        let foreign_uid = unsafe { libc::getuid() } + 1;
        let err = prepare_socket_dir(dir.path(), foreign_uid).unwrap_err();
        assert!(err.contains("refusing to bind"));
        assert!(err.contains("owned by uid"));
    }

    /// Running as root the ownership check is relaxed, but a world-writable
    /// parent must still be refused (any local user could replace the
    /// socket).
    #[test]
    fn socket_dir_rejects_world_writable_parent_for_root() {
        let dir = tempfile::tempdir().unwrap();
        let world_writable = dir.path().join("world_writable");
        fs_err::create_dir(&world_writable).unwrap();
        fs_err::set_permissions(
            &world_writable,
            Permissions::from_mode(0o777),
        )
        .unwrap();
        let err = prepare_socket_dir(&world_writable, 0).unwrap_err();
        assert!(err.contains("world-writable"));
    }

    /// A non-directory in the parent position must be refused with a clear
    /// error instead of a confusing `bind` failure.
    #[test]
    fn socket_dir_rejects_non_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("regular_file");
        fs_err::write(&file, b"not a directory").unwrap();
        let err =
            prepare_socket_dir(&file, unsafe { libc::getuid() }).unwrap_err();
        assert!(err.contains("not a directory"));
    }

    /// A missing parent is created with mode `0700`, independent of the
    /// process umask, so other users cannot even traverse into it.
    #[test]
    fn socket_dir_creates_missing_parent_as_0700() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        prepare_socket_dir(&nested, unsafe { libc::getuid() }).unwrap();
        let mode = fs_err::metadata(&nested).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    /// An existing directory we own (e.g. a systemd `XDG_RUNTIME_DIR`) is
    /// used as-is; its mode must not be rewritten.
    #[test]
    fn socket_dir_keeps_mode_of_existing_dir() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        fs_err::set_permissions(dir.path(), Permissions::from_mode(0o755))
            .unwrap();
        prepare_socket_dir(dir.path(), unsafe { libc::getuid() }).unwrap();
        let mode = fs_err::metadata(dir.path()).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    /// `connect_error` maps the two "no daemon" I/O kinds to friendly
    /// messages and falls through to the raw error otherwise.
    #[test]
    fn connect_error_classifies() {
        let not_found = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(
            <UnixEndpoint as Endpoint>::connect_error(&not_found)
                .contains("no daemon is running")
        );

        let refused =
            std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        assert!(
            <UnixEndpoint as Endpoint>::connect_error(&refused)
                .contains("older")
        );
    }

    /// A relative `XDG_RUNTIME_DIR` is rejected so it cannot re-root the
    /// socket into the CWD (R3-9).
    #[test]
    fn socket_path_rejects_relative_xdg_runtime_dir() {
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "relative/dir");
        }
        let path = socket_path();
        assert!(path.is_absolute(), "fallback path must be absolute");
        assert!(!path.as_os_str().is_empty());
    }

    /// An empty `XDG_RUNTIME_DIR` is rejected (R3-9).
    #[test]
    fn socket_path_rejects_empty_xdg_runtime_dir() {
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "");
        }
        let path = socket_path();
        assert!(path.is_absolute());
    }

    /// An absolute `XDG_RUNTIME_DIR` is used directly (R3-9 happy path).
    #[test]
    fn socket_path_uses_absolute_xdg_runtime_dir() {
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", dir.path().to_str().unwrap());
        }
        let path = socket_path();
        assert_eq!(path, dir.path().join(SOCKET_NAME));
    }

    // -----------------------------------------------------------------------
    // Client-side peer-uid verification (R3-4)
    // -----------------------------------------------------------------------

    /// `verify_server_uid` admits the same-uid server (the normal case: a
    /// user-run daemon) via a loopback socketpair.
    #[test]
    fn verify_server_uid_accepts_own_uid() {
        let (client, _server) = UnixStream::pair().unwrap();
        // The peer of a socketpair is this very process.
        verify_server_uid(&client, unsafe { libc::getuid() }).unwrap();
    }

    /// `verify_server_uid` rejects a peer whose uid is neither root nor the
    /// caller's. The actual kernel-reported uid of a socketpair peer is always
    /// `getuid()`, so the rejection path is exercised by telling
    /// `verify_server_uid` to expect a different uid. A real foreign-uid peer
    /// cannot be created without privileges, so this is the correct in-sandbox
    /// simulation.
    #[test]
    fn verify_server_uid_rejects_foreign_uid() {
        let (client, _server) = UnixStream::pair().unwrap();
        let foreign_uid = unsafe { libc::getuid() } + 1;
        let err = verify_server_uid(&client, foreign_uid).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("neither root nor the current uid"));
    }

    // -----------------------------------------------------------------------
    // Client connect + timeout integration (R3-4)
    // -----------------------------------------------------------------------

    /// The full client [`connect`] path (peer verification + timeout setting)
    /// must not break a normal round-trip: an echo server is bound at the
    /// resolved socket path, and `Endpoint::connect` succeeds, verifies the
    /// uid, and completes the exchange within the 1 s client timeout.
    #[test]
    fn client_connect_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", dir.path().to_str().unwrap());
        }
        let path = socket_path();

        let listener = match bind_at(&path) {
            Ok(listener) => listener,
            Err(e) => {
                eprintln!(
                    "skipping client_connect_round_trips: cannot bind ({e})"
                );
                return;
            }
        };
        std::thread::spawn(move || serve(listener, echo));

        // Give the server thread a moment to enter the accept loop.
        std::thread::sleep(Duration::from_millis(50));

        let mut conn = <UnixEndpoint as Endpoint>::connect().unwrap();
        let command = b"SET-LOG-LEVEL debug";
        frame::write_frame(&mut conn, command).unwrap();
        let reply = frame::read_payload(&mut conn, MAX).unwrap();
        assert_eq!(reply, command);
    }

    /// A server that accepts but never responds must not hang the CLI: after
    /// the 1 s client read timeout, a read surfaces as an error instead of
    /// blocking indefinitely (R3-4).
    #[test]
    fn client_connect_times_out_on_stalled_server() {
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", dir.path().to_str().unwrap());
        }
        let path = socket_path();

        let listener = match bind_at(&path) {
            Ok(listener) => listener,
            Err(e) => {
                eprintln!(
                    "skipping client_connect_times_out_on_stalled_server: \
                     cannot bind ({e})"
                );
                return;
            }
        };

        // Accept the connection and hold it open without reading or writing,
        // simulating a daemon that has crashed or is hung mid-accept.
        let server_thread = std::thread::spawn(move || {
            if let Ok((_stream, _)) = listener.accept() {
                // Keep the connection alive but do nothing — never send a
                // reply, never close. The client's read timeout must fire.
                std::thread::sleep(Duration::from_secs(10));
            }
        });

        std::thread::sleep(Duration::from_millis(50));

        let start = std::time::Instant::now();
        let mut conn = <UnixEndpoint as Endpoint>::connect().unwrap();

        // The server never replies; reading must fail after ~1 s, not hang.
        let mut buf = [0u8; 16];
        let result = std::io::Read::read(&mut conn, &mut buf);
        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "read from a stalled server should time out, not succeed"
        );
        // The timeout must fire well before the server's 10 s sleep, proving
        // the client-side timeout (not the server's) is what bounds the wait.
        assert!(
            elapsed < Duration::from_secs(5),
            "timeout took too long: {elapsed:?}"
        );

        drop(server_thread);
    }
}
