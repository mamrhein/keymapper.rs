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
//! the virtual keyboard.  One connection at a time: one console user maps to
//! one keymapperd, so a second connection waits.

use std::{
    io::BufReader,
    os::unix::{
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use super::{
    config_dir::console_uid, emit::emit_native_key, ipc_frame,
    karabiner_client::KarabinerClient,
};

/// Directory holding the virtkbdd IPC socket.
const SOCKET_DIR: &str = "/var/run/virtkbdd";

/// Name of the IPC socket within [`SOCKET_DIR`].
const SOCKET_NAME: &str = "keymapperd.sock";

/// Poll timeout for the listener, so shutdown signals are observed promptly.
const POLL_TIMEOUT_MS: i32 = 500;

/// The macOS `struct xid` returned by `getpeereid(2)`.
#[repr(C)]
struct Xid {
    xi_uid: libc::uid_t,
    xi_euid: libc::uid_t,
    xi_gid: libc::gid_t,
}

// The `libc` crate's `getpeereid` binding uses the NetBSD signature; macOS
// passes a `struct xid`, so declare the correct prototype here.
unsafe extern "C" {
    fn getpeereid(socket: libc::c_int, peercred: *mut Xid) -> libc::c_int;
}

/// Run the virtkbdd IPC server until a shutdown signal is received.
///
/// Creates and `chown`s the socket, then loops: poll the listener (so signals
/// are observed), accept, verify the peer, and emit each decoded batch.  On a
/// connection close it returns to accept (keymapperd reconnects on its own).
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

    // Defense in depth: the socket is owned by the console user with mode
    // 0600, so only that user can connect.  The `getpeereid` check on accept
    // is authoritative (file permissions can be raced).
    if let Some(uid) = console_uid() {
        chown_socket(&socket_path, uid);
    }
    set_mode(&socket_path, 0o600);

    eprintln!("virtkbdd listening on {}", socket_path.display());

    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }

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
                eprintln!("keymapperd connected (uid {peer})");
                handle_connection(stream, conn);
            }
            (Some(peer), Some(console)) => {
                eprintln!(
                    "rejecting connection from uid {peer} (console uid is \
                     {console})"
                );
            }
            _ => {
                // No console user (headless): reject to be safe.
                eprintln!("rejecting connection: no console user");
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

/// Return the real uid of the connected peer, via `getpeereid(2)`.
fn peer_uid(stream: &UnixStream) -> Option<libc::uid_t> {
    let mut xid = Xid {
        xi_uid: 0,
        xi_euid: 0,
        xi_gid: 0,
    };
    let status = unsafe { getpeereid(stream.as_raw_fd(), &mut xid) };
    if status != 0 {
        return None;
    }
    Some(xid.xi_uid)
}

/// Read frames from a keymapperd connection and emit each batch in order.
fn handle_connection(stream: UnixStream, conn: &KarabinerClient) {
    let mut reader = BufReader::new(stream);
    loop {
        match ipc_frame::decode_stream(&mut reader) {
            Ok(keys) => {
                for key in &keys {
                    emit_native_key(conn, key);
                }
            }
            // A clean close (or a mid-frame close) ends the connection;
            // keymapperd reconnects on its own.
            Err(ipc_frame::IpcFrameError::Eof) => break,
            Err(e) => {
                eprintln!("IPC frame error: {e}; closing connection");
                break;
            }
        }
    }
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
        eprintln!(
            "failed to chown {} to uid {uid}: {}",
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
        eprintln!(
            "failed to chmod {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
}
