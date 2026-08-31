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
/// consumer-page media keys, F13–F20, and JIS-specific keys).
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
        10 => 0x64, // IsoExtra (ISO Section)
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
        46 => 0x10, // M
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
        62 => 0xE4, // RightControl
        56 => 0xE1, // LeftShift
        60 => 0xE5, // RightShift
        58 => 0xE2, // LeftAlt
        61 => 0xE6, // RightAlt
        55 => 0xE3, // LeftCommand
        54 => 0xE7, // RightCommand
        57 => 0x39, // CapsLock
        // Navigation
        115 => 0x4A, // Home
        116 => 0x4B, // PageUp
        119 => 0x4D, // End
        121 => 0x4E, // PageDown
        126 => 0x52, // UpArrow
        125 => 0x51, // DownArrow
        123 => 0x50, // LeftArrow
        124 => 0x4F, // RightArrow
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
        24 => 0x2E, // Equal
        33 => 0x2F, // BracketLeft
        30 => 0x31, // BracketRight
        42 => 0x30, // Backslash
        39 => 0x34, // Quote
        50 => 0x35, // Grave
        43 => 0x36, // Comma
        47 => 0x38, // Period
        44 => 0x37, // Slash
        // Numpad
        65 => 0x63, // NumpadDecimal
        67 => 0x55, // NumpadMultiply
        69 => 0x57, // NumpadPlus
        71 => 0x65, // NumpadClear
        75 => 0x54, // NumpadDivide
        76 => 0x58, // NumpadEnter
        78 => 0x56, // NumpadMinus
        81 => 0x67, // NumpadEqual
        82 => 0x62, // Numpad0
        83 => 0x59, // Numpad1
        84 => 0x5A, // Numpad2
        85 => 0x5B, // Numpad3
        86 => 0x5C, // Numpad4
        87 => 0x5D, // Numpad5
        88 => 0x5E, // Numpad6
        89 => 0x5F, // Numpad7
        90 => 0x60, // Numpad8
        91 => 0x61, // Numpad9
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
        assert_eq!(cg_keycode_to_hid_usage(62), Some(0xE4)); // RightControl
        assert_eq!(cg_keycode_to_hid_usage(56), Some(0xE1)); // LeftShift
        assert_eq!(cg_keycode_to_hid_usage(60), Some(0xE5)); // RightShift
        assert_eq!(cg_keycode_to_hid_usage(58), Some(0xE2)); // LeftAlt
        assert_eq!(cg_keycode_to_hid_usage(61), Some(0xE6)); // RightAlt
        assert_eq!(cg_keycode_to_hid_usage(55), Some(0xE3)); // LeftCommand
        assert_eq!(cg_keycode_to_hid_usage(54), Some(0xE7)); // RightCommand
        assert_eq!(cg_keycode_to_hid_usage(57), Some(0x39)); // CapsLock
    }

    #[test]
    fn unknown_code_returns_none() {
        assert_eq!(cg_keycode_to_hid_usage(0xFFFF), None);
    }

    #[test]
    fn maps_numpad_keys() {
        assert_eq!(cg_keycode_to_hid_usage(65), Some(0x63)); // NumpadDecimal
        assert_eq!(cg_keycode_to_hid_usage(67), Some(0x55)); // NumpadMultiply
        assert_eq!(cg_keycode_to_hid_usage(69), Some(0x57)); // NumpadPlus
        assert_eq!(cg_keycode_to_hid_usage(71), Some(0x65)); // NumpadClear
        assert_eq!(cg_keycode_to_hid_usage(75), Some(0x54)); // NumpadDivide
        assert_eq!(cg_keycode_to_hid_usage(76), Some(0x58)); // NumpadEnter
        assert_eq!(cg_keycode_to_hid_usage(78), Some(0x56)); // NumpadMinus
        assert_eq!(cg_keycode_to_hid_usage(81), Some(0x67)); // NumpadEqual
        assert_eq!(cg_keycode_to_hid_usage(82), Some(0x62)); // Numpad0
        assert_eq!(cg_keycode_to_hid_usage(83), Some(0x59)); // Numpad1
        assert_eq!(cg_keycode_to_hid_usage(84), Some(0x5A)); // Numpad2
        assert_eq!(cg_keycode_to_hid_usage(85), Some(0x5B)); // Numpad3
        assert_eq!(cg_keycode_to_hid_usage(86), Some(0x5C)); // Numpad4
        assert_eq!(cg_keycode_to_hid_usage(87), Some(0x5D)); // Numpad5
        assert_eq!(cg_keycode_to_hid_usage(88), Some(0x5E)); // Numpad6
        assert_eq!(cg_keycode_to_hid_usage(89), Some(0x5F)); // Numpad7
        assert_eq!(cg_keycode_to_hid_usage(90), Some(0x60)); // Numpad8
        assert_eq!(cg_keycode_to_hid_usage(91), Some(0x61)); // Numpad9
    }

    #[test]
    fn maps_navigation_keys() {
        assert_eq!(cg_keycode_to_hid_usage(115), Some(0x4A)); // Home
        assert_eq!(cg_keycode_to_hid_usage(116), Some(0x4B)); // PageUp
        assert_eq!(cg_keycode_to_hid_usage(119), Some(0x4D)); // End
        assert_eq!(cg_keycode_to_hid_usage(121), Some(0x4E)); // PageDown
        assert_eq!(cg_keycode_to_hid_usage(126), Some(0x52)); // UpArrow
        assert_eq!(cg_keycode_to_hid_usage(125), Some(0x51)); // DownArrow
        assert_eq!(cg_keycode_to_hid_usage(123), Some(0x50)); // LeftArrow
        assert_eq!(cg_keycode_to_hid_usage(124), Some(0x4F)); // RightArrow
    }

    #[test]
    fn maps_punctuation_keys() {
        assert_eq!(cg_keycode_to_hid_usage(27), Some(0x2D)); // Minus
        assert_eq!(cg_keycode_to_hid_usage(24), Some(0x2E)); // Equal
        assert_eq!(cg_keycode_to_hid_usage(33), Some(0x2F)); // BracketLeft
        assert_eq!(cg_keycode_to_hid_usage(30), Some(0x31)); // BracketRight
        assert_eq!(cg_keycode_to_hid_usage(42), Some(0x30)); // Backslash
        assert_eq!(cg_keycode_to_hid_usage(39), Some(0x34)); // Quote
        assert_eq!(cg_keycode_to_hid_usage(50), Some(0x35)); // Grave
        assert_eq!(cg_keycode_to_hid_usage(43), Some(0x36)); // Comma
        assert_eq!(cg_keycode_to_hid_usage(47), Some(0x38)); // Period
        assert_eq!(cg_keycode_to_hid_usage(44), Some(0x37)); // Slash
    }

    #[test]
    fn maps_iso_extra() {
        assert_eq!(cg_keycode_to_hid_usage(10), Some(0x64)); // IsoExtra
    }

    #[test]
    fn full_resolves_delete() {
        assert_eq!(cg_keycode_to_hid_usage_full(117), Some(HidUsage::Delete));
    }

    #[test]
    fn full_resolves_numpad() {
        assert_eq!(
            cg_keycode_to_hid_usage_full(76),
            Some(HidUsage::NumpadEnter)
        );
        assert_eq!(cg_keycode_to_hid_usage_full(82), Some(HidUsage::Numpad0));
    }
}
