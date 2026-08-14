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
//! the DriverKit virtual HID keyboard (`driverkit` feature) or via CGEvent
//! posting (non-driverkit builds).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
#[cfg(not(feature = "driverkit"))]
use objc2_core_graphics::{CGEventSource, CGEventSourceStateID};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag::register,
};

#[cfg(feature = "driverkit")]
use super::hid_socket::HidSocket;

/// Start keyboard input capture via IOKit device seizure.
///
/// Discovers physical keyboards, seizes each one, and creates per-device
/// `IOHIDQueue` instances for event delivery.  The CFRunLoop is polled
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

    #[cfg(feature = "driverkit")]
    {
        // Open the virtual HID keyboard.  Fail fast if the driver is not
        // loaded — no CGEvent fallback in this mode.
        let socket = HidSocket::discover_and_open().map_err(|e| {
            format!(
                "DriverKit HID driver not available ({e}). Load the \
                 KeyMapperDriver extension or build without the `driverkit` \
                 feature."
            )
        })?;

        // SAFETY: HidSocket is used only on the main thread (CFRunLoop),
        // so the lack of Send/Sync is not a concern.
        #[allow(clippy::arc_with_non_send_sync)]
        let handle = super::iokit_hid::start_iohid_seizure_mapping(
            lookup,
            Arc::new(socket),
        )
        .map_err(|e| format!("IOKit HID device seizure failed: {e}"))?;

        run_event_loop(handle, shutdown)
    }

    #[cfg(not(feature = "driverkit"))]
    {
        let source =
            CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
                .ok_or("Failed to create CGEventSource")?;

        let handle =
            super::iokit_hid::start_iohid_seizure_mapping(lookup, source)
                .map_err(|e| {
                    format!("IOKit HID device seizure failed: {e}")
                })?;

        run_event_loop(handle, shutdown)
    }
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
