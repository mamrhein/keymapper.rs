// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use objc2_core_foundation::{CFRetained, CFRunLoop, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGKeyCode,
};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag::register,
};

#[cfg(feature = "driverkit")]
use super::hid_socket::{HidSocket, build_keyboard_report, modifier_to_hid};
use crate::daemon::mapping_cache::NativeKey;

// ---------------------------------------------------------------------------
// Key event emission — CGEvent-based output (shared by IOHIDManager and
// non-driverkit builds)
// ---------------------------------------------------------------------------

/// Emit a single `NativeKey` as a chord: hold modifiers, press base,
/// release base, release modifiers in reverse order.
///
/// When the `driverkit` feature is enabled and a `HidSocket` is available,
/// emits a single USB HID keyboard report instead of individual CGEvents.
/// Falls back to CGEvent posting if the driver is not loaded or emission
/// fails.
#[cfg(not(feature = "driverkit"))]
pub(crate) fn emit_key_event(
    source: &CFRetained<CGEventSource>,
    native_key: &NativeKey,
) {
    emit_cg_event_chord(source, native_key);
}

#[cfg(feature = "driverkit")]
pub(crate) fn emit_key_event(
    source: &CFRetained<CGEventSource>,
    hid_socket: &Option<HidSocket>,
    native_key: &NativeKey,
) {
    // Prefer the virtual HID device if available.
    if let Some(socket) = hid_socket {
        if let Ok(report) = build_keyboard_report(
            modifier_to_hid(native_key.modifiers),
            Some(native_key.base as CGKeyCode),
        ) {
            if socket.send_report(&report).is_ok() {
                return;
            }

            eprintln!("HID socket emission failed, falling back to CGEvent");
        }
    }

    // Fallback to CGEvent if driver is not available or report failed.
    emit_cg_event_chord(source, native_key);
}

/// Post a `NativeKey` as individual CGEvent key-down/key-up events.
fn emit_cg_event_chord(
    source: &CFRetained<CGEventSource>,
    native_key: &NativeKey,
) {
    let mut pressed_modifiers: Vec<CGKeyCode> = Vec::new();

    // Map modifier bit positions to CGKeyCodes.
    let modifier_bits = [
        (0, 59), // LeftControl
        (1, 62), // RightControl
        (2, 56), // LeftShift
        (3, 60), // RightShift
        (4, 58), // LeftAlt
        (5, 61), // RightAlt
        (6, 55), // LeftCommand
        (7, 54), // RightCommand
    ];

    // Press modifiers.
    for (bit, code) in modifier_bits {
        if (native_key.modifiers >> bit) & 1 == 1 {
            if let Some(e) =
                CGEvent::new_keyboard_event(Some(source), code, true)
            {
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
            }
            pressed_modifiers.push(code);
            thread::sleep(Duration::from_millis(1));
        }
    }

    // Press base key.
    if let Some(e) = CGEvent::new_keyboard_event(
        Some(source),
        native_key.base as CGKeyCode,
        true,
    ) {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
    }
    thread::sleep(Duration::from_millis(1));

    // Release base key.
    if let Some(e) = CGEvent::new_keyboard_event(
        Some(source),
        native_key.base as CGKeyCode,
        false,
    ) {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
    }
    thread::sleep(Duration::from_millis(1));

    // Release modifiers.
    for code in pressed_modifiers.into_iter() {
        if let Some(e) = CGEvent::new_keyboard_event(Some(source), code, false)
        {
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

// ---------------------------------------------------------------------------
// IOHIDManager event-loop entry point
// ---------------------------------------------------------------------------

/// Start keyboard input capture via IOHIDManager.
///
/// IOHIDManager is the sole input capture mechanism.  Unlike CGEventTap, it
/// delivers events per-device, providing the `IOHIDDeviceRef` in each
/// callback. This gives direct access to device properties (location ID) that
/// can be resolved against the `keyboard_registry` for keyboard filtering.
///
/// CGEvent is still used for output emission (`CGEvent::post`) and the sandbox
/// monitor tap, but only IOHIDManager is used for input.
pub fn start_mapping(
    lookup: Arc<parking_lot::RwLock<dyn crate::daemon::state::Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let source =
        CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .ok_or("Failed to create CGEventSource")?;

    let shutdown = Arc::new(AtomicBool::new(false));
    register(SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    register(SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");

    match super::ioh_device::start_iohid_mapping(
        lookup,
        source,
        shutdown.clone(),
    ) {
        super::ioh_device::IOHidResult::Active(handle, shutdown_flag) => {
            run_event_loop(handle, shutdown_flag)?;
        }
        super::ioh_device::IOHidResult::Unavailable(reason) => {
            return Err(format!(
                "IOHIDManager unavailable: {reason}. Input capture requires \
                 IOHIDManager."
            )
            .into());
        }
    }

    Ok(())
}

/// Run the CFRunLoop until the shutdown flag is set.
fn run_event_loop(
    handle: super::ioh_device::IOHidHandle,
    shutdown: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Poll the run loop in default mode until shutdown is signaled.
    // `kCFRunLoopCommonModes` is a pseudo-mode that cannot be passed to
    // CFRunLoopRunInMode; `kCFRunLoopDefaultMode` is a member of the common
    // modes set and receives the tap events.
    while !shutdown.load(Ordering::Acquire) {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.5, true);
    }

    println!("Shutdown signal received. Cleaning up...");
    drop(handle);

    Ok(())
}
