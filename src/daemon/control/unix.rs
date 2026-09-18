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
//! can connect.

use std::{
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
};

use log::{debug, info, warn};

use super::{IoStream, handle_connection};

/// The application name, used in the fallback endpoint directory.
const APP_NAME: &str = "keymapperd";

/// The control socket file name.
const SOCKET_NAME: &str = "keymapperd.sock";

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

/// Bind the endpoint at *path*, removing any stale socket first.
fn bind_at(path: &Path) -> Result<UnixListener, String> {
    if let Some(parent) = path.parent() {
        fs_err::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Remove a stale socket left by a previous (crashed) instance; a fresh
    // bind otherwise fails with `AddressInUse`.
    let _ = fs_err::remove_file(path);

    let listener = UnixListener::bind(path).map_err(|e| e.to_string())?;
    // Owner-only: the socket is the auth mechanism, so restrict it to the
    // daemon's uid (the current user). Best-effort, as in the virtkbdd IPC
    // server.
    let _ =
        fs_err::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
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
                "control socket unavailable ({e}); runtime log-level control \
                 is disabled"
            );
            return;
        }
    };
    info!("control socket listening on {}", path.display());

    if let Err(e) = std::thread::Builder::new()
        .name("control-socket".into())
        .spawn(move || serve(listener))
    {
        warn!("failed to spawn the control-socket thread: {e}");
    }
}

/// Accept connections and serve one command per connection.
fn serve(listener: UnixListener) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        if let Err(e) = handle_connection(&mut stream) {
            // The peer is a well-behaved CLI; a failure is almost always the
            // peer closing early, so a debug note is enough.
            debug!("control-socket connection ended: {e}");
        }
    }
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
