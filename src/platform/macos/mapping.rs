// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Keyboard input capture via a CGEventTap on macOS.
//!
//! A `kCGHIDEventTap` observes every keyboard event at the earliest point in
//! the input pipeline.  Each event is translated to a [`HidUsage`] and handed
//! to the shared decision core: unmapped keys are passed through unchanged,
//! mapped keys are swallowed and their outputs handed to the virtkbdd IPC
//! client for re-emission through the DriverKit virtual keyboard.  The tap's
//! mach port is scheduled on the main CFRunLoop, which is polled until a
//! shutdown signal (SIGINT or SIGTERM) is received.
//!
//! This runs in the user domain (keymapperd): a CGEventTap requires a
//! WindowServer connection, which a root daemon cannot have.  It needs the
//! Input Monitoring and Accessibility TCC grants; without them, tap creation
//! fails and a clear, actionable error is logged.

use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation,
    CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy, CGEventType,
};
use parking_lot::{Mutex, RwLock};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag::register,
};

use super::{ipc_client::IpcClient, keycode::keycode_to_hid_usage};
use crate::{
    common::{hid_usage::HidUsage, keyboard::KeyboardSpecifier},
    daemon::{
        decision::{Decision, DecisionContext},
        mapping_cache::NativeKey,
        state::Lookup,
    },
};

/// Start keyboard input capture via a CGEventTap.
///
/// Creates a `kCGHIDEventTap` that observes keyboard events, decides each one
/// with the shared decision core, and re-emits mapped outputs through the
/// virtkbdd IPC client.  The CFRunLoop is polled until a shutdown signal
/// (SIGINT or SIGTERM) is received.
///
/// `keyboard_filter` is accepted for a uniform platform signature but ignored
/// in this phase: CGEvents do not expose the originating device, so lookups
/// pass `device_id = None` and per-keyboard filters are skipped.
///
/// `ready_signal` is invoked once the tap is live; it is injected by the
/// caller so this module stays free of test-specific side effects.
pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    #[allow(unused_variables)] keyboard_filter: Option<Vec<KeyboardSpecifier>>,
    ready_signal: Option<Box<dyn FnOnce() + Send>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Register signal handlers for graceful shutdown.
    let shutdown = Arc::new(AtomicBool::new(false));
    register(SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    register(SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");

    // Start the virtkbdd IPC client (spawns the writer thread).  When
    // virtkbdd is unreachable the reachability flag stays false and every key
    // passes through natively, so a dead emitter never breaks typing.
    let ipc = IpcClient::start()?;

    // Create the decision context.  `device_id` is `None` on macOS because
    // CGEvents cannot be correlated with an IOKit device.
    let decision = DecisionContext::new(lookup, None);

    // Build the tap context.  The callback is a plain function pointer, so all
    // state travels through the refcon; the context is a local that stays
    // alive for the run loop's duration, and its address is passed as the
    // refcon.  It is only ever touched on the main run-loop thread, so no
    // `Arc` (and thus no `Send + Sync`) is required.  The tap port is a
    // placeholder here and stored after creation (no events flow until the
    // run loop starts, so this is race-free).
    let mut ctx = TapContext {
        decision: Mutex::new(decision),
        tx: ipc.sender(),
        reachable: ipc.reachable_flag(),
        tap_port: std::ptr::null(),
    };
    let refcon = &mut ctx as *mut TapContext as *mut c_void;

    // Observe key-down, key-up, and modifier (flags-changed) events at the
    // earliest tap point, so a swallowed event never reaches the WindowServer.
    let mask: CGEventMask = (CGEventType::KeyDown.0
        | CGEventType::KeyUp.0
        | CGEventType::FlagsChanged.0) as u64;

    let tap_port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(tap_callback),
            refcon,
        )
    }
    .ok_or_else(|| {
        "failed to create the CGEventTap. Grant keymapperd Input Monitoring \
         and Accessibility access in System Settings → Privacy & Security, \
         then restart it."
            .to_string()
    })?;

    // Store the port so the callback can re-enable the tap if the system
    // disables it.  Safe: no events flow until the run loop starts below.
    ctx.tap_port = &*tap_port as *const CFMachPort;

    // Schedule the tap's mach port on the main run loop so its callbacks fire.
    let source = CFMachPort::new_run_loop_source(None, Some(&tap_port), 0)
        .ok_or("failed to create the tap run-loop source")?;
    CFRunLoop::current()
        .ok_or("failed to get the current run loop")?
        .add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });

    // The tap is live, so the daemon can now process events.
    if let Some(signal) = ready_signal {
        signal();
    }

    run_event_loop(&shutdown);

    // The context and port are dropped here, after the run loop has ended, so
    // no callback can fire on freed memory.  The OS removes the tap when the
    // process exits.
    Ok(())
}

/// Poll the CFRunLoop until the shutdown flag is set.
fn run_event_loop(shutdown: &Arc<AtomicBool>) {
    // `kCFRunLoopDefaultMode` is a member of the common modes set and receives
    // the tap's mach-port callbacks.
    while !shutdown.load(Ordering::Acquire) {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.5, true);
    }

    println!("Shutdown signal received. Cleaning up...");
}

/// Compute the down/up state of a `FlagsChanged` event from its usage and
/// flag mask.
///
/// The eight held modifiers map to their HID modifier bits, and the state is
/// read from the corresponding flag.  CapsLock is a toggle key with no
/// modifier bit, whose state is the alpha-shift flag; it is still a valid
/// trigger key, so it must reach the decision core (macOS delivers it only as
/// `FlagsChanged`, never as key-down/key-up).  Returns `None` for usages that
/// are not mappable keys (Fn, etc.).
fn flags_changed_state(usage: HidUsage, flags: CGEventFlags) -> Option<bool> {
    match HidUsage::hid_usage_to_modifier_bit(usage) {
        Some(bit) => Some(match bit {
            0 | 4 => flags.contains(CGEventFlags::MaskControl),
            1 | 5 => flags.contains(CGEventFlags::MaskShift),
            2 | 6 => flags.contains(CGEventFlags::MaskAlternate),
            _ => flags.contains(CGEventFlags::MaskCommand), // 3 | 7
        }),
        None if usage == HidUsage::CapsLock => {
            Some(flags.contains(CGEventFlags::MaskAlphaShift))
        }
        None => None,
    }
}

/// The state shared with the CGEventTap callback via its refcon.
struct TapContext {
    /// The decision core, guarded because `decide` takes `&mut self`.
    decision: Mutex<DecisionContext>,
    /// The virtkbdd batch sender (fire-and-forget).
    tx: mpsc::SyncSender<Vec<NativeKey>>,
    /// The virtkbdd reachability flag.
    reachable: Arc<AtomicBool>,
    /// The tap's mach port, so the callback can re-enable a disabled tap.
    /// Set after `tap_create`, before the run loop starts.
    tap_port: *const CFMachPort,
}

/// The CGEventTap callback.
///
/// Runs on the main run-loop thread and must stay fast: a lock-read lookup
/// plus a `try_send`, no I/O.  Returns the event to pass it through, or null
/// to swallow it.
unsafe extern "C-unwind" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    refcon: *mut c_void,
) -> *mut CGEvent {
    let ctx = unsafe { &*(refcon as *const TapContext) };

    // The system disables taps that block (or when secure input is active).
    // Re-enable and pass the event through.
    if event_type == CGEventType::TapDisabledByTimeout
        || event_type == CGEventType::TapDisabledByUserInput
    {
        if !ctx.tap_port.is_null() {
            unsafe { CGEvent::tap_enable(&*ctx.tap_port, true) };
        }
        return event.as_ptr();
    }

    let cg_event = unsafe { event.as_ref() };
    let keycode = CGEvent::integer_value_field(
        Some(cg_event),
        CGEventField::KeyboardEventKeycode,
    ) as u16;

    let (usage, is_down) = match event_type {
        CGEventType::KeyDown => (keycode_to_hid_usage(keycode), true),
        CGEventType::KeyUp => (keycode_to_hid_usage(keycode), false),
        // Modifier presses arrive here, not as key-down/key-up.  Resolve the
        // usage from the keycode; `is_down` is read from the event's flag
        // mask.
        CGEventType::FlagsChanged => {
            let Some(usage) = keycode_to_hid_usage(keycode) else {
                return event.as_ptr(); // Fn, etc.: no HID equivalent.
            };
            let flags = CGEvent::flags(Some(cg_event));
            let Some(is_down) = flags_changed_state(usage, flags) else {
                return event.as_ptr();
            };
            (Some(usage), is_down)
        }
        _ => return event.as_ptr(),
    };

    // No keyboard-page HID equivalent (media keys, F13+): pass through.
    let Some(usage) = usage else {
        return event.as_ptr();
    };

    let reachable = ctx.reachable.load(Ordering::Acquire);
    let decision = {
        let mut d = ctx.decision.lock();
        d.decide(usage, is_down, reachable)
    };

    match decision {
        Decision::Pass => event.as_ptr(),
        Decision::Emit(outputs) => {
            // Fire-and-forget; drop the batch if the channel is full.
            let _ = ctx.tx.try_send(outputs);
            std::ptr::null_mut()
        }
        Decision::Swallow => std::ptr::null_mut(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A held modifier's state is read from its flag: set means down,
    /// cleared means up.  Right-side modifiers share the flag with their
    /// left-side twin.
    #[test]
    fn modifier_state_from_flag_mask() {
        assert_eq!(
            flags_changed_state(
                HidUsage::LeftControl,
                CGEventFlags::MaskControl
            ),
            Some(true)
        );
        assert_eq!(
            flags_changed_state(HidUsage::LeftControl, CGEventFlags::empty()),
            Some(false)
        );
        assert_eq!(
            flags_changed_state(HidUsage::RightShift, CGEventFlags::MaskShift),
            Some(true)
        );
        assert_eq!(
            flags_changed_state(HidUsage::RightAlt, CGEventFlags::empty()),
            Some(false)
        );
    }

    /// CapsLock is a toggle key with no modifier bit; its state is the
    /// alpha-shift flag, and it must still reach the decision core (macOS
    /// delivers it only as `FlagsChanged`).
    #[test]
    fn capslock_state_from_alpha_shift_flag() {
        assert_eq!(
            flags_changed_state(
                HidUsage::CapsLock,
                CGEventFlags::MaskAlphaShift
            ),
            Some(true)
        );
        assert_eq!(
            flags_changed_state(HidUsage::CapsLock, CGEventFlags::empty()),
            Some(false)
        );
    }

    /// A usage that is neither a held modifier nor CapsLock is not mappable
    /// via `FlagsChanged`.
    #[test]
    fn non_modifier_usage_is_not_mappable() {
        assert_eq!(
            flags_changed_state(HidUsage::A, CGEventFlags::empty()),
            None
        );
    }
}
