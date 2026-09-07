// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The virtkbdd daemon: root-owned virtual-keyboard emitter on macOS.
//!
//! virtkbdd runs as root (a LaunchDaemon) because it owns the Karabiner
//! DriverKit virtual-HID socket, which is root-only.  It connects to the
//! Karabiner daemon, waits (bounded) for the virtual keyboard to become ready,
//! and then serves the keymapperd IPC socket, emitting each mapped-output
//! batch through the virtual keyboard.  It needs no configuration of its own.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag::register,
};

use super::{
    ipc_server,
    karabiner_client::{KarabinerClient, OUTPUT_KEYBOARD_IDENTITY},
};

/// Bounded wait for the virtual keyboard to become ready at startup, so the
/// first mapped events are not dropped.  The client keeps retrying in the
/// background regardless, so if the timeout elapses we continue anyway.
const READY_TIMEOUT: Duration = Duration::from_secs(25);

/// Start the virtkbdd daemon and block until a shutdown signal is received.
///
/// Connects to the Karabiner DriverKit daemon, waits (bounded) for the virtual
/// keyboard to become ready, and then runs the keymapperd IPC server, emitting
/// each decoded batch through the virtual keyboard.
pub fn start_virtkbd() -> Result<(), Box<dyn std::error::Error>> {
    // Register signal handlers for graceful shutdown.
    let shutdown = Arc::new(AtomicBool::new(false));
    register(SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    register(SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");

    // Connect to the Karabiner DriverKit VirtualHIDDevice daemon.  The client
    // spawns a background thread that retries the connection until the daemon
    // is reachable, so this returns immediately.
    let client =
        KarabinerClient::connect(OUTPUT_KEYBOARD_IDENTITY).map_err(|e| {
            format!(
                "Karabiner client failed to start ({e}). Install and \
                 activate the Karabiner DriverKit package."
            )
        })?;

    // Wait for the virtual keyboard to become ready so that the first emitted
    // events are not dropped, but stay responsive to shutdown.  The client
    // keeps retrying in the background, so if the timeout elapses we continue
    // anyway and reports flow once it connects.
    let ready_deadline = Instant::now() + READY_TIMEOUT;
    while !client.is_ready() {
        if shutdown.load(Ordering::Acquire) {
            return Ok(());
        }
        if Instant::now() >= ready_deadline {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    if !client.is_ready() {
        eprintln!(
            "Karabiner virtual keyboard not ready after {}s; continuing and \
             retrying in the background",
            READY_TIMEOUT.as_secs()
        );
    }

    // Serve keymapperd until a shutdown signal is received.
    ipc_server::run_server(&client, shutdown)?;

    println!("Shutdown signal received. Cleaning up...");
    Ok(())
}
