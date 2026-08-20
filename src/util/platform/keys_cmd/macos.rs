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

use crate::common::hid_usage::HidUsage;

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
        if keycode == ESCAPE_KEYCODE
            && flags.contains(CGEventFlags::MaskControl)
        {
            request_exit();
            return event.as_ptr();
        }

        // Print the key information for non-modifier keys.  Modifier keyDown
        // events may fire alongside flagsChanged; when both arrive we let
        // flagsChanged handle it (it fires first and carries the keycode too).
        if !is_modifier_keycode(keycode) {
            let (name, code_str) = cg_keycode_to_description(keycode);

            println!("{name}: {code_str}");
        }
    } else if event_type == CGEventType::FlagsChanged {
        handle_flags_changed(keycode, flags);
    }

    // Pass the event through (don't consume it).
    event.as_ptr()
}

/// CGKeyCode for Escape (0x35).  Used to detect Control+Escape exit.
const ESCAPE_KEYCODE: u16 = 53;

/// Check whether a keycode corresponds to a modifier key.
fn is_modifier_keycode(code: u16) -> bool {
    let modifier_codes = [
        // Standard modifier CGKeyCodes:
        59, // LeftControl
        62, // RightControl
        56, // LeftShift
        60, // RightShift
        58, // LeftAlt (Option)
        61, // RightAlt (Right Option)
        55, // LeftCommand
        54, // RightCommand
        57, // CapsLock
    ];
    modifier_codes.contains(&code)
}

/// Convert a CGKeyCode to a human-readable (name, code) pair.
///
/// Uses HID usage codes as the primary display format.  Falls back to
/// raw CGKeyCode for unrecognized keys.
fn cg_keycode_to_description(code: u16) -> (String, String) {
    // Try to convert CGKeyCode to a HID usage id.
    if let Some(usage_id) = cg_keycode_to_hid_usage(code)
        && let Some(hu) = HidUsage::keyboard(usage_id)
    {
        return (hu.as_str().to_string(), format!("0x{usage_id:02X}"));
    }

    (format!("Unknown({code})"), format!("{code}"))
}

/// Convert a macOS CGKeyCode to its USB HID Keyboard/Keypad usage id.
fn cg_keycode_to_hid_usage(code: u16) -> Option<u16> {
    Some(match code {
        // Letters
        0 => 0x04,  // A
        1 => 0x16,  // S
        2 => 0x07,  // D
        3 => 0x09,  // F
        4 => 0x0B,  // H
        5 => 0x0A,  // G
        6 => 0x1D,  // Z
        7 => 0x1B,  // X
        8 => 0x06,  // C
        9 => 0x19,  // V
        10 => 0x63, // IsoExtra
        11 => 0x05, // B
        12 => 0x14, // Q
        13 => 0x1A, // W
        14 => 0x08, // E
        15 => 0x15, // R
        16 => 0x1C, // Y
        17 => 0x17, // T
        31 => 0x12, // O
        32 => 0x18, // U
        34 => 0x0C, // I
        35 => 0x13, // P
        37 => 0x0F, // L
        38 => 0x0D, // J
        40 => 0x0E, // K
        41 => 0x33, // Semicolon
        45 => 0x11, // N
        46 => 0x10, // M,
        // Numbers
        18 => 0x1E, // 1
        19 => 0x1F, // 2
        20 => 0x20, // 3
        21 => 0x21, // 4
        23 => 0x22, // 5
        22 => 0x23, // 6
        26 => 0x24, // 7
        28 => 0x25, // 8
        25 => 0x26, // 9
        29 => 0x27, // 0
        // Edit keys
        36 => 0x28, // Return
        51 => 0x2A, // Backspace
        53 => 0x29, // Escape
        48 => 0x2B, // Tab
        49 => 0x2C, // Space
        // Modifiers
        59 => 0xE0, // LeftControl
        62 => 0xE1, // RightControl
        56 => 0xE2, // LeftShift
        60 => 0xE3, // RightShift
        58 => 0xE4, // LeftAlt
        61 => 0xE5, // RightAlt
        55 => 0xE6, // LeftCommand
        54 => 0xE7, // RightCommand
        57 => 0x39, // CapsLock
        // Navigation
        115 => 0x4A, // Home
        119 => 0x4D, // End
        116 => 0x4E, // PageUp
        121 => 0x4F, // PageDown
        126 => 0x52, // UpArrow
        125 => 0x51, // DownArrow
        123 => 0x50, // LeftArrow
        124 => 0x4B, // RightArrow
        // Function keys
        122 => 0x3A, // F1
        120 => 0x3B, // F2
        99 => 0x3C,  // F3
        118 => 0x3D, // F4
        96 => 0x3E,  // F5
        97 => 0x3F,  // F6
        98 => 0x40,  // F7
        100 => 0x41, // F8
        101 => 0x42, // F9
        109 => 0x43, // F10
        103 => 0x44, // F11
        111 => 0x45, // F12
        // Punctuation
        27 => 0x2D, // Minus
        24 => 0x2F, // Equal
        33 => 0x31, // BracketLeft
        30 => 0x32, // BracketRight
        42 => 0x31, // Backslash
        39 => 0x34, // Quote
        50 => 0x35, // Grave
        43 => 0x36, // Comma
        47 => 0x38, // Period
        44 => 0x37, // Slash
        _ => return None,
    })
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

    if is_down {
        let (name, code_str) = cg_keycode_to_description(keycode);
        println!("{name}: {code_str}");
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
