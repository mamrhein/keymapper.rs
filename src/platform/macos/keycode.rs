// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS CGKeyCode to HID usage conversion.
//!
//! Maps the virtual key codes used by CoreGraphics events (`CGKeyCode`, the
//! `kVK_*` constants) to USB HID Keyboard/Keypad usage ids.  Shared by the
//! daemon's capture path (which observes injected events via a CGEventTap)
//! and the `keys probe` CLI command.

use crate::common::hid_usage::HidUsage;

/// Convert a macOS CGKeyCode to its USB HID Keyboard/Keypad usage id.
///
/// Returns `None` for codes with no keyboard-page HID equivalent (e.g. the
/// consumer-page media keys, which have no `kVK_*` constant).
pub fn cg_keycode_to_hid_usage(code: u16) -> Option<u16> {
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
        36 => 0x28,  // Return
        51 => 0x2A,  // Backspace
        117 => 0x4C, // Delete (forward delete)
        53 => 0x29,  // Escape
        48 => 0x2B,  // Tab
        49 => 0x2C,  // Space
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

/// Convert a macOS CGKeyCode to a full `HidUsage`.
///
/// Convenience wrapper over [`cg_keycode_to_hid_usage`] that also resolves the
/// usage id to a `HidUsage` on the keyboard page.  Returns `None` for codes
/// with no keyboard-page HID equivalent.
pub fn cg_keycode_to_hid_usage_full(code: u16) -> Option<HidUsage> {
    cg_keycode_to_hid_usage(code).and_then(HidUsage::keyboard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_edit_keys() {
        assert_eq!(cg_keycode_to_hid_usage(36), Some(0x28)); // Return
        assert_eq!(cg_keycode_to_hid_usage(51), Some(0x2A)); // Backspace
        assert_eq!(cg_keycode_to_hid_usage(117), Some(0x4C)); // Delete
        assert_eq!(cg_keycode_to_hid_usage(53), Some(0x29)); // Escape
        assert_eq!(cg_keycode_to_hid_usage(48), Some(0x2B)); // Tab
        assert_eq!(cg_keycode_to_hid_usage(49), Some(0x2C)); // Space
    }

    #[test]
    fn maps_letters_and_modifiers() {
        assert_eq!(cg_keycode_to_hid_usage(0), Some(0x04)); // A
        assert_eq!(cg_keycode_to_hid_usage(12), Some(0x14)); // Q
        assert_eq!(cg_keycode_to_hid_usage(59), Some(0xE0)); // LeftControl
        assert_eq!(cg_keycode_to_hid_usage(55), Some(0xE6)); // LeftCommand
        assert_eq!(cg_keycode_to_hid_usage(57), Some(0x39)); // CapsLock
    }

    #[test]
    fn unknown_code_returns_none() {
        assert_eq!(cg_keycode_to_hid_usage(0xFFFF), None);
    }

    #[test]
    fn full_resolves_delete() {
        assert_eq!(cg_keycode_to_hid_usage_full(117), Some(HidUsage::Delete));
    }
}
