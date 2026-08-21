// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Keyboard input capture via IOKit device seizure on macOS.
//!
//! Each physical keyboard is opened with `kIOHIDOptionsTypeSeizeDevice` so
//! only this process receives its events.  Mapped output is emitted through
//! the DriverKit virtual HID keyboard.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag::register,
};

use super::hid_socket::HidSocket;

/// Start keyboard input capture via IOKit device seizure.
///
/// Discovers physical keyboards, seizes each one, and creates per-device
/// `IOHIDQueue` instances for event delivery.  Mapped output is emitted
/// through the DriverKit virtual HID keyboard.  The CFRunLoop is polled
/// until a shutdown signal (SIGINT or SIGTERM) is received.
pub fn start_mapping(
    lookup: Arc<parking_lot::RwLock<dyn crate::daemon::state::Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Register signal handlers for graceful shutdown.
    let shutdown = Arc::new(AtomicBool::new(false));
    register(SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    register(SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");

    // Open the virtual HID keyboard.  Fail fast if the driver is not loaded.
    let socket = HidSocket::discover_and_open().map_err(|e| {
        format!(
            "DriverKit HID driver not available ({e}). Load the \
             KeyMapperDriver extension."
        )
    })?;

    // HidSocket holds only an IOService connection handle (a Mach port
    // right), so it is Send + Sync.  In practice it is used only on the
    // main thread (CFRunLoop), where the queue callbacks emit reports.
    let handle = super::iokit_hid::start_iohid_seizure_mapping(
        lookup,
        Arc::new(socket),
    )
    .map_err(|e| format!("IOKit HID device seizure failed: {e}"))?;

    // The socket is open and the seizure mapping is live, so the daemon can
    // now process events.  Signal readiness for the e2e harness.
    crate::daemon::signal_ready();

    run_event_loop(handle, shutdown)
}

/// Poll the CFRunLoop until the shutdown flag is set.
fn run_event_loop(
    handle: super::iokit_hid::SeizureHandle,
    shutdown: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    // `kCFRunLoopCommonModes` is a pseudo-mode that cannot be passed to
    // CFRunLoopRunInMode; `kCFRunLoopDefaultMode` is a member of the common
    // modes set and receives the queue callbacks.
    while !shutdown.load(Ordering::Acquire) {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.5, true);
    }

    println!("Shutdown signal received. Cleaning up...");
    drop(handle);

    Ok(())
}
