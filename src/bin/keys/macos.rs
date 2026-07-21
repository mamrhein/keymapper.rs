// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS implementation of `keymapper keys probe`.

use keymapper::os::Key;
use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopCommonModes};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventTapLocation,
    CGEventTapOptions, CGEventTapPlacement, CGEventType, CGKeyCode,
};

/// Probe for key presses using a CGEventTap.
pub fn probe() {
    println!("Press keys to see their names and codes.");
    println!("Press Control+Escape to exit.\n");

    let mask: u64 =
        (1u64 << CGEventType::KeyDown.0) | (1u64 << CGEventType::KeyUp.0);

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

    // Poll the run loop until Control+Escape triggers loop termination.
    loop {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopCommonModes }, 0.5, true);

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

    let is_key_down = event_type == CGEventType::KeyDown;

    // Check for Control+Escape exit condition.
    if is_key_down {
        let flags = unsafe { CGEvent::flags(Some(event.as_ref())) };
        if keycode == Key::Escape.as_native()
            && flags.contains(CGEventFlags::MaskControl)
        {
            request_exit();
            return event.as_ptr();
        }

        // Skip printing modifier key events.
        if keycode == Key::LeftControl.as_native()
            || keycode == Key::RightControl.as_native()
            || keycode == Key::LeftShift.as_native()
            || keycode == Key::RightShift.as_native()
            || keycode == Key::LeftAlt.as_native()
            || keycode == Key::RightAlt.as_native()
            || keycode == Key::LeftCommand.as_native()
            || keycode == Key::RightCommand.as_native()
        {
            return event.as_ptr();
        }

        // Print the key information.
        let (name, code_str) = if let Some(key) = Key::from_native(keycode) {
            (key.as_str().to_string(), format!("{}", key.as_native()))
        } else {
            (format!("Unknown({keycode})"), format!("{keycode}"))
        };

        println!("{name}: {code_str}");
    }

    // Pass the event through (don't consume it).
    event.as_ptr()
}

// ---------------------------------------------------------------------------
// Exit signalling between the callback thread and the main poll loop
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, Ordering};

static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

fn request_exit() {
    EXIT_REQUESTED.store(true, Ordering::SeqCst);
}

fn should_exit() -> bool {
    EXIT_REQUESTED.load(Ordering::SeqCst)
}
