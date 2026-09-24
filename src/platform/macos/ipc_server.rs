// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The virtkbdd side of the keymapperd IPC: a root-owned socket server.
//!
//! virtkbdd runs as root and owns the DriverKit virtual keyboard.  It listens
//! on a UNIX stream socket, verifies each peer with `getpeereid(2)` (rejecting
//! any uid that is not the console user), and emits each decoded batch through
//! the virtual keyboard.  The console-user check is re-applied for the life of
//! each connection, so a connection that outlives a fast user switch cannot
//! inject keystrokes into the new console user's session.  One connection at a
//! time: one console user maps to one keymapperd, so a second connection
//! waits.

use std::{
    io::{BufReader, ErrorKind},
    os::unix::{
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use log::{error, info, warn};

use super::{
    config_dir::console_uid, emit::emit_native_key, ipc_frame,
    karabiner_client::KarabinerClient,
};
use crate::daemon::mapping_cache::NativeKey;

/// Directory holding the virtkbdd IPC socket.
const SOCKET_DIR: &str = "/var/run/virtkbdd";

/// Name of the IPC socket within [`SOCKET_DIR`].
const SOCKET_NAME: &str = "keymapperd.sock";

/// Poll timeout for the listener, so shutdown signals are observed promptly.
const POLL_TIMEOUT_MS: i32 = 500;

/// Read timeout for an accepted keymapperd connection.
///
/// Bounds how long one peer can occupy the single-connection accept loop,
/// so a stalled peer cannot wedge the server indefinitely.  The timeout
/// is not an error: an idle connection whose console user is still
/// current keeps waiting.  Its purpose is to wake the connection loop
/// periodically so it can re-check the console user when no frames are
/// arriving.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Run the virtkbdd IPC server until a shutdown signal is received.
///
/// Creates the socket and keeps it owned by the current console user, then
/// loops: poll the listener (so signals are observed), accept, verify the
/// peer, and emit each decoded batch while re-verifying that the peer is
/// still the console user.  On a connection close it returns to accept
/// (keymapperd reconnects on its own).
///
/// virtkbdd starts at boot, before any user logs in, so the console user —
/// and thus the socket's owner — changes over the daemon's lifetime.  The
/// ownership is re-applied in the accept loop whenever it changes, so the
/// logged-in user's keymapperd can always connect.
pub fn run_server(
    conn: &KarabinerClient,
    shutdown: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket_dir = Path::new(SOCKET_DIR);
    fs_err::create_dir_all(socket_dir)?;
    set_mode(socket_dir, 0o755);

    let socket_path = socket_dir.join(SOCKET_NAME);
    // Remove a stale socket left by a previous (crashed) instance.
    let _ = fs_err::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;

    // The socket starts mode 0600 so it is never world-writable, even before
    // the console user is known.
    set_mode(&socket_path, 0o600);

    // Defense in depth: the socket is owned by the console user, so only that
    // user can connect.  The `getpeereid` check on accept is authoritative
    // (file permissions can be raced).  `socket_owner` tracks the uid the
    // socket was last chowned to so the accept loop can re-apply ownership
    // when the console user changes (login, logout, fast user switching).
    let mut socket_owner: Option<libc::uid_t> = None;
    apply_socket_ownership(&socket_path, &mut socket_owner);

    info!("Service virtkbdd listening on {}", socket_path.display());

    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }

        // Keep the socket owned by the current console user so their
        // keymapperd can connect; a no-op unless the uid changed.
        apply_socket_ownership(&socket_path, &mut socket_owner);

        // Poll the listener with a timeout so SIGINT/SIGTERM are observed.
        if !wait_for_peer(&listener) {
            continue;
        }

        let Ok((stream, _)) = listener.accept() else {
            continue;
        };

        // Verify the peer is the console user; reject (and drop) otherwise.
        match (peer_uid(&stream), console_uid()) {
            (Some(peer), Some(console)) if peer == console => {
                info!("Service keymapperd connected (uid {peer})");
                // Bound every read so a stalled peer cannot wedge the
                // accept loop, and so `handle_connection` wakes
                // periodically to re-check the console user even while
                // idle.
                if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
                    warn!(
                        "Failed to set the IPC read timeout on {}: {}; \
                         rejecting connection",
                        socket_path.display(),
                        std::io::Error::last_os_error()
                    );
                    continue;
                }
                handle_connection(stream, peer, &|keys| {
                    for key in keys {
                        emit_native_key(conn, key);
                    }
                });
            }
            (Some(peer), Some(console)) => {
                warn!(
                    "Rejecting connection from uid {peer} (console uid is \
                     {console})"
                );
            }
            _ => {
                // No console user (headless): reject to be safe.
                warn!("Rejecting connection: no console user");
            }
        }
    }

    Ok(())
}

/// Block until the listener has a pending connection or the timeout elapses.
fn wait_for_peer(listener: &UnixListener) -> bool {
    let mut pollfd = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // A negative return is a signal or error; treat it as "no peer" so the
    // loop re-checks the shutdown flag.
    let result = unsafe { libc::poll(&mut pollfd, 1, POLL_TIMEOUT_MS) };
    result > 0
}

/// The macOS `struct xid` exchanged with `getpeereid(2)`.
///
/// macOS 27 changed the prototype from `(int, struct xid *)` to
/// `(int, uid_t *, gid_t *)`.  Passing the first and third fields of this
/// struct as the two out-pointers is correct under both ABIs: the classic
/// form writes all three fields through the first pointer, while the new
/// form writes the euid and the gid to offsets 0 and 8.  Only offset 0 is
/// read back, so the call stays within the struct on every release.
#[repr(C)]
struct Xid {
    xi_uid: libc::uid_t,
    xi_euid: libc::uid_t,
    xi_gid: libc::gid_t,
}

/// Return the uid of the connected peer, via `getpeereid(2)`.
///
/// Offset 0 of [`Xid`] holds the effective uid on macOS 27+ and the real
/// uid on earlier releases.  For a normal user process such as keymapperd
/// the two are equal, and it is the identity that matters here.
fn peer_uid(stream: &UnixStream) -> Option<libc::uid_t> {
    let mut xid = Xid {
        xi_uid: 0,
        xi_euid: 0,
        xi_gid: 0,
    };
    let status = unsafe {
        libc::getpeereid(stream.as_raw_fd(), &mut xid.xi_uid, &mut xid.xi_gid)
    };
    if status != 0 {
        return None;
    }
    Some(xid.xi_uid)
}

/// Read frames from a keymapperd connection and emit each batch in order.
///
/// The peer's uid was verified at accept; a fast user switch can change
/// the console user while the connection stays open, so the check is
/// repeated with every arriving frame (a cheap `fstat`, see
/// [`console_uid`]) and on every read timeout while the connection is
/// idle.  When the console user is no longer the peer the connection is
/// dropped, so root never emits the previous user's keystrokes into the
/// new user's session; keymapperd reconnects on its own and is
/// re-verified at accept.
fn handle_connection(
    stream: UnixStream,
    peer_uid: libc::uid_t,
    emit: &impl Fn(&[NativeKey]),
) {
    let mut reader = BufReader::new(stream);
    loop {
        match ipc_frame::decode_stream(&mut reader) {
            Ok(keys) => {
                if console_uid() != Some(peer_uid) {
                    info!(
                        "Console user changed while uid {peer_uid} was \
                         connected; dropping the connection"
                    );
                    break;
                }
                emit(&keys);
            }
            // A clean close (or a mid-frame close) ends the connection;
            // keymapperd reconnects on its own.
            Err(ipc_frame::IpcFrameError::Eof) => break,
            // A read timeout only means no frame arrived in time.
            // Re-check the console user (it may have changed while the
            // connection was idle) and keep waiting if it did not.
            Err(ipc_frame::IpcFrameError::Io(ref e))
                if e.kind() == ErrorKind::WouldBlock =>
            {
                if console_uid() != Some(peer_uid) {
                    info!(
                        "Console user changed while uid {peer_uid} was idle; \
                         dropping the connection"
                    );
                    break;
                }
            }
            Err(e) => {
                error!("IPC frame error: {e}; closing connection");
                break;
            }
        }
    }
}

/// (Re-)apply the current console user's ownership to the socket.
///
/// `last_owner` tracks the uid the socket was last chowned to, so the
/// (best-effort) `chown`/`chmod` syscalls only run when the console user
/// actually changes.  A `None` console user (headless) leaves the socket as
/// is; the `getpeereid` check rejects peers in that case anyway.
fn apply_socket_ownership(path: &Path, last_owner: &mut Option<libc::uid_t>) {
    let Some(uid) = console_uid() else {
        return;
    };
    if *last_owner == Some(uid) {
        return;
    }
    chown_socket(path, uid);
    set_mode(path, 0o600);
    *last_owner = Some(uid);
}

/// `chown` the socket to the console user (group wheel).  Best-effort: a
/// failure is logged but not fatal, because the `getpeereid` check is the
/// authoritative gate.
fn chown_socket(path: &Path, uid: libc::uid_t) {
    let Ok(c_path) =
        std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
    else {
        return;
    };
    if unsafe { libc::chown(c_path.as_ptr(), uid, 0) } != 0 {
        warn!(
            "Failed to chown {} to uid {uid}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
}

/// Set a path's permission bits.  Best-effort, as in [`chown_socket`].
fn set_mode(path: &Path, mode: libc::mode_t) {
    let Ok(c_path) =
        std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
    else {
        return;
    };
    if unsafe { libc::chmod(c_path.as_ptr(), mode) } != 0 {
        warn!(
            "Failed to chmod {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        os::unix::net::UnixStream,
        sync::{Arc, Mutex, mpsc},
    };

    use super::*;
    use crate::common::hid_usage::HidUsage;

    fn key() -> NativeKey {
        NativeKey {
            modifiers: 0,
            usage: HidUsage::A,
        }
    }

    /// Drain the client end until the server closes the connection, so a
    /// test never hangs if the server keeps the socket open.
    fn read_until_eof(client: &mut UnixStream) {
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut buf = [0u8; 64];
        loop {
            match client.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => panic!("client read failed: {e}"),
            }
        }
    }

    /// A frame from the current console user is emitted, and the loop
    /// exits cleanly on the peer close.
    #[test]
    fn emits_frames_from_the_console_user() {
        let Some(console) = console_uid() else {
            eprintln!("skipping: no console user on this host");
            return;
        };
        let (server, mut client) = UnixStream::pair().unwrap();
        client.write_all(&ipc_frame::encode(&[key()])).unwrap();

        let emitted = Arc::new(Mutex::new(0usize));
        let emitted_thread = Arc::clone(&emitted);
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            server
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            handle_connection(server, console, &|keys| {
                *emitted_thread.lock().unwrap() += keys.len();
            });
            let _ = done_tx.send(());
        });

        // The frame is consumed, then dropping the client yields EOF and
        // the connection loop returns.
        std::thread::sleep(Duration::from_millis(100));
        drop(client);
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(*emitted.lock().unwrap(), 1);
    }

    /// A frame from a peer that is no longer (or was never) the console
    /// user is not emitted, and the connection is dropped.
    #[test]
    fn drops_connection_when_console_user_changed() {
        let Some(console) = console_uid() else {
            eprintln!("skipping: no console user on this host");
            return;
        };
        // A uid that cannot be the console user.
        let stale_uid = console.wrapping_add(1_000);
        let (server, mut client) = UnixStream::pair().unwrap();
        client.write_all(&ipc_frame::encode(&[key()])).unwrap();

        let emitted = Arc::new(Mutex::new(0usize));
        let emitted_thread = Arc::clone(&emitted);
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            server
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            handle_connection(server, stale_uid, &|keys| {
                *emitted_thread.lock().unwrap() += keys.len();
            });
            let _ = done_tx.send(());
        });

        // The server must close the connection instead of emitting.
        read_until_eof(&mut client);
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(*emitted.lock().unwrap(), 0);
    }
}
