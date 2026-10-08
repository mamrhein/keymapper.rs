// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The keymapperd side of the virtkbdd IPC: a fire-and-forget writer.
//!
//! The CGEventTap callback hands each mapped-output batch to [`IpcClient`] via
//! a bounded channel and never blocks.  A dedicated writer thread owns the
//! socket: it connects to virtkbdd (retrying on failure), verifies the
//! server's uid is root via `getpeereid(2)` for mutual authentication, writes
//! frames in order, and maintains the reachability flag that the decision
//! core consults.  When virtkbdd is unreachable every key passes through
//! natively (mappings simply inactive), so a dead emitter never breaks
//! typing.
//!
//! Ordering is preserved end to end by the single stream, virtkbdd's single
//! reader, and `KarabinerClient`'s internal mpsc.

use std::{
    io::Write,
    os::unix::{io::AsRawFd, net::UnixStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use log::{info, warn};

use super::ipc_frame;
use crate::keymap_core::mapping_cache::NativeKey;

/// Path of the virtkbdd IPC socket.
const SOCKET_PATH: &str = "/var/run/virtkbdd/keymapperd.sock";

/// The macOS `struct xid` exchanged with `getpeereid(2)`.
///
/// macOS 27 changed the prototype from `(int, struct xid *)` to
/// `(int, uid_t *, gid_t *)`. Passing the first and third fields of this
/// struct as the two out-pointers is correct under both ABIs: the classic
/// form writes all three fields through the first pointer, while the new
/// form writes the euid and the gid to offsets 0 and 8. Only offset 0 is
/// read back, so the call stays within the struct on every release.
///
/// (Same layout trick as the virtkbdd IPC server's `peer_uid` and the
/// control socket's `peer_uid_macos`.)
#[repr(C)]
struct Xid {
    xi_uid: libc::uid_t,
    xi_euid: libc::uid_t,
    xi_gid: libc::gid_t,
}

/// Bounded channel capacity, in batches.  The tap callback never blocks: when
/// the channel is full a batch is dropped and counted.
const CHANNEL_CAPACITY: usize = 512;

/// Interval between reconnection attempts.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

/// Per-batch write timeout.
const WRITE_TIMEOUT: Duration = Duration::from_millis(15_000);

/// Poll interval for draining the channel while connected.  Kept short so
/// batches are written promptly instead of waiting for the next batch.
const DRAIN_INTERVAL: Duration = Duration::from_millis(50);

/// Verify that the connected virtkbdd server is running as root.
///
/// virtkbdd always runs as root, so the peer uid must be 0. This provides
/// mutual authentication: the server verifies the client's uid via
/// `getpeereid`, and the client verifies the server's uid here. If the socket
/// path is compromised (e.g., an attacker places a socket at the same path),
/// the uid check fails and the connection is dropped.
fn verify_server_uid(stream: &UnixStream) -> Result<(), String> {
    let mut xid = Xid {
        xi_uid: 0,
        xi_euid: 0,
        xi_gid: 0,
    };
    let status = unsafe {
        libc::getpeereid(stream.as_raw_fd(), &mut xid.xi_uid, &mut xid.xi_gid)
    };
    if status != 0 {
        return Err(format!(
            "getpeereid: {}",
            std::io::Error::last_os_error()
        ));
    }
    if xid.xi_uid != 0 {
        return Err(format!(
            "server uid is {}, expected root (0)",
            xid.xi_uid
        ));
    }
    Ok(())
}

/// The keymapperd-side IPC client.
///
/// A single instance is created per daemon; the tap callback's context holds a
/// clone of the batch sender and the reachability flag (not a clone of this
/// handle), so [`Drop`] fires exactly once when the daemon shuts down.
pub struct IpcClient {
    tx: mpsc::SyncSender<Vec<NativeKey>>,
    reachable: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

impl IpcClient {
    /// Start the writer thread and return a client handle.
    pub fn start() -> Result<Self, Box<dyn std::error::Error>> {
        let (tx, rx) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let reachable = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));

        let reachable_thread = Arc::clone(&reachable);
        let shutdown_thread = Arc::clone(&shutdown);

        thread::Builder::new()
            .name("virtkbdd-writer".into())
            .spawn(move || writer_loop(rx, reachable_thread, shutdown_thread))
            .map_err(|e| {
                format!("failed to spawn the virtkbdd writer thread: {e}")
            })?;

        Ok(Self {
            tx,
            reachable,
            shutdown,
        })
    }

    /// A clone of the batch sender, for the tap callback's context.
    pub fn sender(&self) -> mpsc::SyncSender<Vec<NativeKey>> {
        self.tx.clone()
    }

    /// The reachability flag, shared with the tap callback's context.  The
    /// decision core loads it and passes the result to `decide`, so that an
    /// unreachable emitter yields native pass-through.
    pub fn reachable_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.reachable)
    }
}

impl Drop for IpcClient {
    fn drop(&mut self) {
        // Ask the writer thread to exit; it stops within one drain or
        // reconnect cycle.  This handle is never cloned, so the flag flips
        // exactly once, when the daemon shuts down.
        self.shutdown.store(true, Ordering::Release);
    }
}

/// The writer thread's main loop: connect, write frames, and retry until
/// shutdown is requested.
fn writer_loop(
    rx: mpsc::Receiver<Vec<NativeKey>>,
    reachable: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) {
    loop {
        if shutdown.load(Ordering::Acquire) {
            return;
        }

        match UnixStream::connect(SOCKET_PATH) {
            Ok(stream) => {
                // Verify the server's uid is root (virtkbdd always runs as
                // root) before sending any keystrokes. This closes the
                // client-side half of mutual authentication: if the socket
                // path is compromised, the uid check fails and the
                // connection is dropped (Finding 5).
                if let Err(e) = verify_server_uid(&stream) {
                    warn!(
                        "Connection to virtkbdd rejected ({e}); retrying in
                         {} ms",
                        RECONNECT_INTERVAL.as_millis()
                    );
                } else {
                    // The socket is live and trusted; mark the emitter
                    // reachable before the first batch is written.
                    reachable.store(true, Ordering::Release);
                    info!("Connection to virtkbdd established");
                    if let Err(e) = write_loop(stream, &rx, &shutdown) {
                        warn!(
                            "Connection to virtkbdd lost ({e}); reconnecting
                             in {} ms",
                            RECONNECT_INTERVAL.as_millis()
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Connection to virtkbdd not reachable ({e}); retrying in \
                     {} ms",
                    RECONNECT_INTERVAL.as_millis()
                );
            }
        }

        reachable.store(false, Ordering::Release);
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        thread::sleep(RECONNECT_INTERVAL);
    }
}

/// Write frames to a live connection until it is lost or shutdown is
/// requested.  Batches are written in order, preserving event sequencing.
fn write_loop(
    mut stream: UnixStream,
    rx: &mpsc::Receiver<Vec<NativeKey>>,
    shutdown: &Arc<AtomicBool>,
) -> std::io::Result<()> {
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;

    loop {
        if shutdown.load(Ordering::Acquire) {
            return Ok(());
        }

        match rx.recv_timeout(DRAIN_INTERVAL) {
            Ok(keys) => {
                let frame = ipc_frame::encode(&keys);
                stream.write_all(&frame)?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}
