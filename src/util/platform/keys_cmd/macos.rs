// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS implementation of `keymapper keys probe`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use objc2_core_foundation::{
    CFMachPort, CFRunLoop, kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventTapLocation,
    CGEventTapOptions, CGEventTapPlacement, CGEventType, CGKeyCode,
};

use crate::platform::Key;

/// Probe for key presses using a CGEventTap.
pub fn probe() {
    let mask: u64 = (1u64 << CGEventType::KeyDown.0)
        | (1u64 << CGEventType::KeyUp.0)
        | (1u64 << CGEventType::FlagsChanged.0);

    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(probe_callback),
            std::ptr::null_mut(),
        )
    };

    let Some(tap) = tap else {
        eprintln!(
            "Failed to create event tap. Verify Accessibility privileges?"
        );
        std::process::exit(1);
    };

    let Some(run_loop_source) =
        CFMachPort::new_run_loop_source(None, Some(&tap), 0)
    else {
        eprintln!("Failed to create run loop source.");
        std::process::exit(1);
    };

    let run_loop = CFRunLoop::current().expect("no current run loop");

    run_loop
        .add_source(Some(&run_loop_source), unsafe { kCFRunLoopCommonModes });

    CGEvent::tap_enable(&tap, true);

    println!("Press keys to see their names and codes.");
    println!("Press Control+Escape to exit.\n");

    // Poll the run loop in default mode until Control+Escape triggers loop
    // termination. `kCFRunLoopCommonModes` is a pseudo-mode that cannot be
    // passed to CFRunLoopRunInMode; `kCFRunLoopDefaultMode` is a member of the
    // common modes set and receives the tap events.
    loop {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.5, true);

        // Check for shutdown signal set by the callback.
        if should_exit() {
            break;
        }
    }

    CGEvent::tap_enable(&tap, false);
}

/// Event-tap callback that prints key info and checks for the exit
/// condition (Control+Escape).
unsafe extern "C-unwind" fn probe_callback(
    _proxy: objc2_core_graphics::CGEventTapProxy,
    event_type: CGEventType,
    event: core::ptr::NonNull<objc2_core_graphics::CGEvent>,
    _user_info: *mut std::ffi::c_void,
) -> *mut objc2_core_graphics::CGEvent {
    let keycode: CGKeyCode = unsafe {
        CGEvent::integer_value_field(
            Some(event.as_ref()),
            CGEventField::KeyboardEventKeycode,
        )
    } as CGKeyCode;

    let flags = unsafe { CGEvent::flags(Some(event.as_ref())) };

    if event_type == CGEventType::KeyDown {
        // Check for Control+Escape exit condition.
        if keycode == Key::Escape.as_native()
            && flags.contains(CGEventFlags::MaskControl)
        {
            request_exit();
            return event.as_ptr();
        }

        // Print the key information for non-modifier keys.  Modifier keyDown
        // events may fire alongside flagsChanged; when both arrive we let
        // flagsChanged handle it (it fires first and carries the keycode too).
        if !is_modifier_keycode(keycode) {
            let (name, code_str) = if let Some(key) = Key::from_native(keycode)
            {
                (key.as_str().to_string(), format!("{}", key.as_native()))
            } else {
                (format!("Unknown({keycode})"), format!("{keycode}"))
            };

            println!("{name}: {code_str}");
        }
    } else if event_type == CGEventType::FlagsChanged {
        handle_flags_changed(keycode, flags);
    }

    // Pass the event through (don't consume it).
    event.as_ptr()
}

/// Check whether a keycode corresponds to a modifier key.
fn is_modifier_keycode(code: u16) -> bool {
    let modifier_codes = [
        Key::LeftControl.as_native(),
        Key::RightControl.as_native(),
        Key::LeftShift.as_native(),
        Key::RightShift.as_native(),
        Key::LeftAlt.as_native(),
        Key::RightAlt.as_native(),
        Key::LeftCommand.as_native(),
        Key::RightCommand.as_native(),
        Key::CapsLock.as_native(),
    ];
    modifier_codes.contains(&code)
}

/// Handle flags-changed events.  The event carries the native keycode of the
/// modifier that changed, allowing left/right distinction even though event
/// flags alone cannot differentiate them.
///
/// Only down-events are reported; releases are silent.
fn handle_flags_changed(keycode: u16, current: CGEventFlags) {
    let prev = PREV_FLAGS.swap(current.bits(), Ordering::SeqCst);

    // Only print when the modifier is pressed (flag transitions from unset to
    // set). Releases are silent, matching non-modifier key behaviour.
    let is_down = current.bits() > prev;

    if is_down && let Some(key) = Key::from_native(keycode) {
        println!("{}: {}", key.as_str(), keycode);
    }
}

// ---------------------------------------------------------------------------
// Exit signalling between the callback thread and the main poll loop
// ---------------------------------------------------------------------------

static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Previous modifier flags from the last flagsChanged event.
static PREV_FLAGS: AtomicU64 = AtomicU64::new(0);

fn request_exit() {
    EXIT_REQUESTED.store(true, Ordering::SeqCst);
}

fn should_exit() -> bool {
    EXIT_REQUESTED.load(Ordering::SeqCst)
}
