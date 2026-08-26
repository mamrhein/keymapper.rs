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
//! Each physical keyboard matching the global filter is opened with
//! `kIOHIDOptionsTypeSeizeDevice` so only this process receives its events.
//! Every key is re-emitted through the DriverKit virtual HID keyboard: mapped
//! keys as their mapped output, unmapped keys forwarded unchanged.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag::register,
};

use super::karabiner_client::KarabinerClient;

/// Start keyboard input capture via IOKit device seizure.
///
/// Discovers physical keyboards, seizes those matching the global keyboard
/// filter (or all of them when no filter is set), and creates per-device
/// `IOHIDQueue` instances for event delivery.  Every key is re-emitted through
/// the DriverKit virtual HID keyboard: mapped keys as their mapped output,
/// unmapped keys forwarded unchanged.  The CFRunLoop is polled until a
/// shutdown signal (SIGINT or SIGTERM) is received.
pub fn start_mapping(
    lookup: Arc<parking_lot::RwLock<dyn crate::daemon::state::Lookup>>,
    keyboard_filter: Option<Vec<crate::common::keyboard::KeyboardSpecifier>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Register signal handlers for graceful shutdown.
    let shutdown = Arc::new(AtomicBool::new(false));
    register(SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    register(SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");

    // Connect to the Karabiner DriverKit VirtualHIDDevice daemon.  The
    // client spawns a background thread that retries the connection until
    // the daemon is reachable, so this returns immediately.
    let client = KarabinerClient::connect().map_err(|e| {
        format!(
            "Karabiner client failed to start ({e}). Install and activate \
             the Karabiner DriverKit package."
        )
    })?;

    // Wait for the virtual keyboard to become ready so that the first
    // injected events are not dropped, but stay responsive to shutdown.  The
    // client keeps retrying in the background, so if the timeout elapses we
    // continue anyway and reports flow once it connects.
    let ready_deadline = Instant::now() + Duration::from_secs(25);
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
            "Karabiner virtual keyboard not ready after 25s; continuing and \
             retrying in the background"
        );
    }

    // Capture mode (e2e only, gated on `KEYMAPPER_CAPTURE`): the daemon
    // already re-emits every key through the virtual keyboard in all modes,
    // so capture mode only adds a CGEventTap to observe injected events, which
    // never reach the seized physical keyboards.  This lets the e2e monitor
    // capture the daemon's output without depending on a focused window.
    // Production behaviour is left untouched.
    let capture =
        std::env::var("KEYMAPPER_CAPTURE").is_ok_and(|v| !v.is_empty());
    if capture {
        eprintln!("macOS: capture mode enabled (KEYMAPPER_CAPTURE).");
    }

    // KarabinerClient is Send + Sync (it holds an mpsc sender and atomic
    // flags), so it can be shared with the queue callbacks.  In practice it
    // is used only on the main thread (CFRunLoop), where the queue callbacks
    // emit reports.
    let client = Arc::new(client);

    let handle = super::iokit_hid::start_iohid_seizure_mapping(
        lookup.clone(),
        client.clone(),
        keyboard_filter.as_deref(),
    )
    .map_err(|e| format!("IOKit HID device seizure failed: {e}"))?;

    // In capture mode, also observe injected events via a CGEventTap.  The
    // e2e injector posts keys with `CGEventPost` at the HID event tap, which
    // enters the CoreGraphics event stream but never reaches the IOKit HID
    // queues of the seized physical keyboards.  The tap closes that gap: it
    // sees the injected events and re-emits them through the virtual keyboard.
    // Physical keyboards are already seized (their events do not reach the CG
    // stream), so there is no double-processing.
    let cgeventtap = if capture {
        Some(
            super::iokit_hid::start_cgeventtap_capture(lookup, client)
                .map_err(|e| format!("CGEventTap capture failed: {e}"))?,
        )
    } else {
        None
    };

    // The seizure mapping is live, so the daemon can now process events.
    // Signal readiness for the e2e harness.
    crate::daemon::signal_ready();

    run_event_loop(handle, cgeventtap, shutdown)
}

/// Poll the CFRunLoop until the shutdown flag is set.
fn run_event_loop(
    handle: super::iokit_hid::SeizureHandle,
    cgeventtap: Option<super::iokit_hid::CgEventTapHandle>,
    shutdown: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    // `kCFRunLoopCommonModes` is a pseudo-mode that cannot be passed to
    // CFRunLoopRunInMode; `kCFRunLoopDefaultMode` is a member of the common
    // modes set and receives the queue callbacks and the CGEventTap events.
    while !shutdown.load(Ordering::Acquire) {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.5, true);
    }

    println!("Shutdown signal received. Cleaning up...");
    drop(handle);
    drop(cgeventtap);

    Ok(())
}
