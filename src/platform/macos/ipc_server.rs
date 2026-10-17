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
//! on a UNIX stream socket, verifies each peer with the audit token (via
//! `LOCAL_PEERTOKEN`, which exposes both the real/audit uid and the effective
//! uid so a `setuid`-root squatter cannot pass an euid-only gate), and
//! emits each decoded batch through the virtual keyboard.  The console-user
//! check is re-applied for the life of each connection, so a connection that
//! outlives a fast user switch cannot inject keystrokes into the new console
//! user's session.  One connection at a time: one console user maps to one
//! keymapperd, so a second connection waits.
//!
//! Peer identity is uid-based (audit token), not code-signing based — the
//! connecting process's PID and executable path are logged for operational
//! visibility but do not by themselves prevent same-uid squatting.

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
    time::{Duration, Instant},
};

use log::{error, info, warn};

use super::{
    config_dir::console_uid, emit::emit_native_key, ipc_frame,
    karabiner_client::KarabinerClient,
};
use crate::{
    common::frame,
    keymap_core::mapping_cache::NativeKey,
    platform::endpoint::{RateLimiter, unix::peer_uid},
};

/// Directory holding the virtkbdd IPC socket.
const SOCKET_DIR: &str = "/var/run/virtkbdd";

/// Name of the IPC socket within [`SOCKET_DIR`].
const SOCKET_NAME: &str = "keymapperd.sock";

/// Sliding-window size for per-uid connection rate limiting at accept time.
const ACCEPT_RATE_WINDOW: Duration = Duration::from_secs(1);

/// Maximum accepted connections per uid within [`ACCEPT_RATE_WINDOW`].
const ACCEPT_RATE_THRESHOLD: usize = 10;

/// Sliding-window size for per-connection frame rate limiting in the emit
/// path.
const FRAME_RATE_WINDOW: Duration = Duration::from_secs(1);

/// Maximum frames accepted per connection within [`FRAME_RATE_WINDOW`].
///
/// Bounds how fast a single peer can flood the emit loop; at 1000 frames/s a
/// squatter cannot saturate the CPU or the virtual keyboard any faster.
const FRAME_RATE_THRESHOLD: usize = 1000;

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
/// peer via its audit token, and emit each decoded batch while re-verifying
/// that the peer is still the console user.  On a connection close it returns
/// to accept (keymapperd reconnects on its own).
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
    // user can connect.  The audit-token check on accept is authoritative
    // (file permissions can be raced).  `socket_owner` tracks the uid the
    // socket was last chowned to so the accept loop can re-apply ownership
    // when the console user changes (login, logout, fast user switching).
    let mut socket_owner: Option<libc::uid_t> = None;
    apply_socket_ownership(&socket_path, &mut socket_owner);

    info!("Service virtkbdd listening on {:?}", socket_path);

    // Per-uid connection rate limiter: a single process cannot flood the
    // accept loop with connections.
    let mut accept_rate_limiter =
        RateLimiter::new(ACCEPT_RATE_WINDOW, ACCEPT_RATE_THRESHOLD);

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

        // Fetch the peer's credentials via the audit token (LOCAL_PEERTOKEN),
        // not `getpeereid(2)`, so a setuid-root squatter cannot pass an
        // euid-only gate. The audit token also yields the peer PID for
        // logging.
        let creds = match peer_uid::peer_creds(&stream) {
            Ok(creds) => creds,
            Err(e) => {
                warn!(
                    "Rejecting connection: peer credential lookup failed \
                     ({e})"
                );
                continue;
            }
        };
        let peer_uid = creds.real_uid;
        let console = match console_uid() {
            Some(c) => c,
            None => {
                // No console user (headless): reject to be safe.
                warn!("Rejecting connection: no console user");
                continue;
            }
        };

        // Per-uid connection rate limit: reject excess connections from the
        // same uid so a squatter cannot wedge the single-connection accept
        // loop.
        if !accept_rate_limiter.check_and_record(peer_uid as u32) {
            warn!("Connection rate limit exceeded for uid {peer_uid}");
            continue;
        }

        if peer_uid != console {
            warn!(
                "Rejecting connection from uid {peer_uid} (console uid is \
                 {console})"
            );
            continue;
        }

        // Log the connecting process's PID and executable path for operational
        // visibility. This does not prevent same-uid squatting, but makes
        // connection attempts auditable.
        let pid_str = match creds.peer_pid.and_then(peer_uid::peer_proc_path) {
            Some(path) => {
                format!("pid {}, path \"{}\"", creds.peer_pid.unwrap(), path)
            }
            None => match creds.peer_pid {
                Some(pid) => format!("pid {pid}"),
                None => "no pid available".to_string(),
            },
        };
        info!("Service keymapperd connected (uid {peer_uid}, {pid_str})");

        // Bound every read so a stalled peer cannot wedge the accept loop,
        // and so `handle_connection` wakes periodically to re-check the
        // console user even while idle.
        if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
            warn!(
                "Failed to set the IPC read timeout on {}: {}; rejecting \
                 connection",
                socket_path.display(),
                std::io::Error::last_os_error()
            );
            continue;
        }
        handle_connection(stream, peer_uid, &|keys| {
            for key in keys {
                emit_native_key(conn, key);
            }
        });
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
///
/// A per-connection frame-rate limiter bounds how fast a squatting peer can
/// flood the emit path. A connection that exceeds the budget is dropped so
/// keymapperd must reconnect (and re-pass the uid + rate limit at accept).
fn handle_connection(
    stream: UnixStream,
    peer_uid: libc::uid_t,
    emit: &impl Fn(&[NativeKey]),
) {
    let mut reader = BufReader::new(stream);
    // A persistent `FrameReader` retains partial header/payload bytes across
    // the read-timeout retries in this loop, so a `SO_RCVTIMEO` firing
    // mid-frame cannot desync the stream (Finding R3-12).  `decode_stream`
    // resumes on it.
    let mut frame_reader =
        frame::FrameReader::new(&mut reader, ipc_frame::MAX_PAYLOAD_LEN);
    // Per-connection frame rate limiter: bounds how fast this peer can emit
    // frames, preventing a squatter from flooding the virtual keyboard.
    let mut frame_limiter =
        FrameRateLimiter::new(FRAME_RATE_WINDOW, FRAME_RATE_THRESHOLD);
    loop {
        match ipc_frame::decode_stream(&mut frame_reader) {
            Ok(keys) => {
                if console_uid() != Some(peer_uid) {
                    info!(
                        "Console user changed while uid {peer_uid} was \
                         connected; dropping the connection"
                    );
                    break;
                }
                // Reject connections that exceed the frame-rate budget. A
                // malicious peer could otherwise flood the emit loop and
                // saturate the CPU or the virtual keyboard.
                if !frame_limiter.check_and_record() {
                    warn!(
                        "Frame rate limit exceeded for uid {peer_uid}; \
                         dropping connection"
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
/// is; the audit-token check rejects peers in that case anyway.
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
/// failure is logged but not fatal, because the audit-token check is the
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
// Frame rate limiter (per-connection)
// ---------------------------------------------------------------------------

/// A lightweight sliding-window frame-rate limiter for the virtkbdd IPC
/// server.
///
/// Unlike [`RateLimiter`] (which is keyed by uid and tracks multiple peers),
/// this limiter is per-connection: it bounds how fast a single peer can push
/// frames through the emit loop.  The connection is single-threaded, so no
/// synchronization is needed — the limiter is created fresh per accepted
/// connection and lives on that thread's stack.
///
/// Old entries outside the window are pruned on every call, so the `Vec` stays
/// bounded by the threshold.
struct FrameRateLimiter {
    window: Duration,
    threshold: usize,
    // Timestamps of frames seen within the current window.  At most
    // `threshold` entries are ever retained; excess frames are rejected
    // before being recorded.
    frames: Vec<Instant>,
}

impl FrameRateLimiter {
    fn new(window: Duration, threshold: usize) -> Self {
        Self {
            window,
            threshold,
            frames: Vec::with_capacity(threshold),
        }
    }

    /// Returns `true` if the frame is allowed, `false` if the rate limit has
    /// been exceeded.
    fn check_and_record(&mut self) -> bool {
        let now = Instant::now();
        let cutoff = now - self.window;
        self.frames.retain(|&ts| ts >= cutoff);
        if self.frames.len() >= self.threshold {
            return false;
        }
        self.frames.push(now);
        true
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
        time::Duration,
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

    /// The frame-rate limiter allows frames up to the threshold, then rejects.
    #[test]
    fn frame_rate_limiter_allows_then_rejects() {
        let mut limiter = FrameRateLimiter::new(Duration::from_secs(1), 5);
        for _ in 0..5 {
            assert!(
                limiter.check_and_record(),
                "frame should be allowed within budget"
            );
        }
        assert!(
            !limiter.check_and_record(),
            "frame should be rejected after budget exhausted"
        );
    }

    /// Old entries are pruned so the limiter recovers after the window
    /// elapses.
    #[test]
    fn frame_rate_limiter_recovers_after_window() {
        let mut limiter = FrameRateLimiter::new(Duration::from_millis(50), 3);
        for _ in 0..3 {
            assert!(limiter.check_and_record());
        }
        assert!(!limiter.check_and_record());
        // Wait for the window to elapse; the limiter should recover.
        std::thread::sleep(Duration::from_millis(60));
        assert!(
            limiter.check_and_record(),
            "limiter should recover after window"
        );
    }
}
