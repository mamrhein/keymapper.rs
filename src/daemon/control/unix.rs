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

use super::{IoStream, handle_connection};

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

/// The control endpoint path.
///
/// `$XDG_RUNTIME_DIR/keymapperd.sock` when the runtime dir is set (the normal
/// case under a per-user systemd session); otherwise a per-user cache
/// directory. Both the daemon and the CLI resolve this the same way from the
/// same environment, so they always agree.
pub(super) fn socket_path() -> PathBuf {
    if let Some(rd) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(rd).join(SOCKET_NAME);
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

/// Bind the endpoint and spawn the accept thread.
pub fn start() {
    let path = socket_path();
    let listener = match bind_at(&path) {
        Ok(listener) => listener,
        Err(e) => {
            // The control socket is a convenience; a bind failure must not
            // stop the daemon. The level then stays at the env-var seed.
            warn!(
                "Control socket unavailable ({e}); runtime log-level control \
                 is disabled"
            );
            return;
        }
    };
    info!("Control socket listening on {}", path.display());

    if let Err(e) = std::thread::Builder::new()
        .name("control-socket".into())
        .spawn(move || serve(listener))
    {
        warn!("Failed to spawn the control-socket thread: {e}");
    }
}

/// Accept connections and serve one command per connection, applying the
/// default [`IO_TIMEOUT`] to each accepted stream.
fn serve(listener: UnixListener) {
    serve_with_timeout(listener, IO_TIMEOUT);
}

/// Accept connections and serve one command per connection, applying
/// *io_timeout* to each accepted stream.
fn serve_with_timeout(listener: UnixListener, io_timeout: Duration) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        // Authoritative peer check, mirroring the `getpeereid` gate in the
        // virtkbdd IPC server: the file mode is the first gate, but kernel
        // credentials are the source of truth, so a permission race (or a
        // mode weakened by an external actor) still cannot admit a foreign
        // user.
        if let Err(e) = authorize_peer(&stream, unsafe { libc::getuid() }) {
            debug!("control-socket rejecting connection: {e}");
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
        if let Err(e) = handle_connection(&mut stream) {
            // The peer is a well-behaved CLI; a failure is almost always the
            // peer closing early, so a debug note is enough.
            debug!("control-socket connection ended: {e}");
        }
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
) -> Result<(), String> {
    let peer = peer_uid(stream)?;
    if peer == 0 || peer == current_uid {
        Ok(())
    } else {
        Err(format!(
            "peer uid {peer} is neither the daemon uid nor root"
        ))
    }
}

/// Return the uid of the connected peer, or an error if the kernel cannot
/// report it (the caller then fails closed).
#[cfg(not(target_os = "macos"))]
fn peer_uid(stream: &UnixStream) -> Result<libc::uid_t, String> {
    use std::{io, os::unix::io::AsRawFd};

    // `SO_PEERCRED` is filled in by the kernel at connect time and cannot
    // be forged by the peer.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(cred).cast::<libc::c_void>(),
            &mut len,
        )
    };
    if status != 0 {
        return Err(format!(
            "getsockopt(SO_PEERCRED): {}",
            io::Error::last_os_error()
        ));
    }
    Ok(cred.uid)
}

/// The macOS `struct xid` exchanged with `getpeereid(2)`.
///
/// macOS 27 changed the prototype from `(int, struct xid *)` to
/// `(int, uid_t *, gid_t *)`. Passing the first and third fields of this
/// struct as the two out-pointers is correct under both ABIs: the classic
/// form writes all three fields through the first pointer, while the new
/// form writes the euid and the gid to offsets 0 and 8. Only offset 0 is
/// read back, so the call stays within the struct on every release.
///
/// (Same layout trick as the virtkbdd IPC server's `peer_uid`.)
#[cfg(target_os = "macos")]
#[repr(C)]
struct Xid {
    xi_uid: libc::uid_t,
    xi_euid: libc::uid_t,
    xi_gid: libc::gid_t,
}

/// Return the uid of the connected peer, via `getpeereid(2)`.
///
/// Offset 0 of [`Xid`] holds the effective uid on macOS 27+ and the real
/// uid on earlier releases. For a normal user process such as keymapperd
/// the two are equal, and it is the identity that matters here.
#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> Result<libc::uid_t, String> {
    use std::{io, os::unix::io::AsRawFd};

    let mut xid = Xid {
        xi_uid: 0,
        xi_euid: 0,
        xi_gid: 0,
    };
    let status = unsafe {
        libc::getpeereid(stream.as_raw_fd(), &mut xid.xi_uid, &mut xid.xi_gid)
    };
    if status != 0 {
        return Err(format!("getpeereid: {}", io::Error::last_os_error()));
    }
    Ok(xid.xi_uid)
}

/// Connect to the endpoint at *path* as a client.
fn connect_at(path: &Path) -> std::io::Result<UnixStream> {
    UnixStream::connect(path)
}

/// Connect to the daemon's control endpoint as a client.
pub(super) fn connect() -> std::io::Result<Box<dyn IoStream>> {
    let stream = connect_at(&socket_path())?;
    Ok(Box::new(stream))
}

/// Convert a failed client connection into a [`ControlError`] the CLI can
/// turn into a clear "daemon not running / older than CLI" message.
pub(super) fn connect_error(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "no daemon is running (or its \
                                         control socket is not at the \
                                         expected path)"
            .to_string(),
        std::io::ErrorKind::ConnectionRefused => "no daemon control socket \
                                                  is listening (the daemon \
                                                  may be older than this CLI)"
            .to_string(),
        _ => e.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

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

    /// The full protocol over a real unix socket: a `serve` thread in a
    /// background thread, a client that connects, sends a level change, and
    /// reads the reply. This exercises framing, dispatch, and the atomic
    /// level store.
    #[test]
    fn control_socket_round_trip() {
        use super::super::{logging, read_frame, write_frame};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SOCKET_NAME);

        let listener = match bind_at(&path) {
            Ok(listener) => listener,
            Err(e) => {
                // Some sandboxes forbid unix domain sockets (EPERM); skip the
                // live round trip there. The codec and dispatch logic are
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
        std::thread::spawn(move || serve(listener));

        let mut stream =
            wait_for_socket(&path, Duration::from_secs(2)).unwrap();
        // A timeout turns a stuck (e.g. unimplemented) server into an error
        // instead of a hung test.
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        write_frame(&mut stream, "SET-LOG-LEVEL debug").unwrap();
        let reply = read_frame(&mut stream).unwrap();
        assert_eq!(reply, "OK debug");

        // Restore the default level so other tests observe it.
        logging::set_level(log::LevelFilter::Info);
    }

    /// A peer that connects and sends nothing must not wedge the
    /// single-threaded accept loop: once its read times out the server
    /// closes the connection and serves the next client.
    #[test]
    fn idle_peer_does_not_wedge_the_server() {
        use super::super::{logging, read_frame, write_frame};

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
            serve_with_timeout(listener, Duration::from_millis(200))
        });

        // A silent peer occupies the loop until its read times out.
        let _idle = wait_for_socket(&path, Duration::from_secs(2)).unwrap();

        // A second client is served once the first has been dropped.
        let mut stream =
            wait_for_socket(&path, Duration::from_secs(2)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write_frame(&mut stream, "SET-LOG-LEVEL debug").unwrap();
        let reply = read_frame(&mut stream).unwrap();
        assert_eq!(reply, "OK debug");

        // Restore the default level so other tests observe it.
        logging::set_level(log::LevelFilter::Info);
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
        assert!(connect_error(&not_found).contains("no daemon is running"));

        let refused =
            std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        assert!(connect_error(&refused).contains("older"));
    }
}
