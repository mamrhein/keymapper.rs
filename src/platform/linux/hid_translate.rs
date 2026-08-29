// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Static translation between HID usages and evdev key codes.
//!
//! The kernel's `hid-input` driver translates HID reports into evdev events
//! inside the kernel, and that translation is not callable from user space.
//! The daemon therefore carries its own static tables:
//!
//! - `hid_usage_to_keycode` — output direction: converts a `HidUsage` to the
//!   evdev `KEY_*` code written to the uinput virtual keyboard.
//! - `keycode_to_hid_usage` — input fallback: converts an evdev `KEY_*` code
//!   back to a `HidUsage` for devices that do not emit `MSC_SCAN` (older
//!   kernels, some virtual devices).  For devices that emit `MSC_SCAN`, the
//!   scan code already encodes the full HID usage as `(page << 16) | id` and
//!   `HidUsage::from_code` resolves it without any table lookup.
//!
//! The tables cover all usages in `HidUsage::ALL` (102 keyboard/keypad
//! entries plus 9 consumer page entries).  Key code values are derived
//! from `include/uapi/linux/input-event-codes.h`.

use crate::common::hid_usage::HidUsage;

/// Map a HID usage to the evdev `KEY_*` code used for emission on the
/// uinput virtual keyboard.
///
/// Returns `None` for usages without a stable evdev equivalent.
pub fn hid_usage_to_keycode(usage: HidUsage) -> Option<u16> {
    match (usage.page(), usage.id()) {
        // --- Keyboard page (0x07) ---
        // Modifiers.
        (0x07, 0xE0) => Some(29),  // KEY_LEFTCTRL
        (0x07, 0xE1) => Some(42),  // KEY_LEFTSHIFT
        (0x07, 0xE2) => Some(56),  // KEY_LEFTALT
        (0x07, 0xE3) => Some(125), // KEY_LEFTMETA
        (0x07, 0xE4) => Some(97),  // KEY_RIGHTCTRL
        (0x07, 0xE5) => Some(54),  // KEY_RIGHTSHIFT
        (0x07, 0xE6) => Some(100), // KEY_RIGHTALT
        (0x07, 0xE7) => Some(126), // KEY_RIGHTMETA
        // Editor / misc.
        (0x07, 0x39) => Some(58),  // KEY_CAPSLOCK
        (0x07, 0x2B) => Some(15),  // KEY_TAB
        (0x07, 0x2C) => Some(57),  // KEY_SPACE
        (0x07, 0x28) => Some(28),  // KEY_ENTER
        (0x07, 0x2A) => Some(14),  // KEY_BACKSPACE
        (0x07, 0x4C) => Some(111), // KEY_DELETE
        (0x07, 0x29) => Some(1),   // KEY_ESC
        // Navigation.
        (0x07, 0x52) => Some(103), // KEY_UP
        (0x07, 0x51) => Some(108), // KEY_DOWN
        (0x07, 0x50) => Some(105), // KEY_LEFT
        (0x07, 0x4F) => Some(106), // KEY_RIGHT
        (0x07, 0x4B) => Some(104), // KEY_PAGEUP
        (0x07, 0x4E) => Some(109), // KEY_PAGEDOWN
        (0x07, 0x4A) => Some(102), // KEY_HOME
        (0x07, 0x4D) => Some(107), // KEY_END
        // Function keys.
        (0x07, 0x3A) => Some(59), // KEY_F1
        (0x07, 0x3B) => Some(60), // KEY_F2
        (0x07, 0x3C) => Some(61), // KEY_F3
        (0x07, 0x3D) => Some(62), // KEY_F4
        (0x07, 0x3E) => Some(63), // KEY_F5
        (0x07, 0x3F) => Some(64), // KEY_F6
        (0x07, 0x40) => Some(65), // KEY_F7
        (0x07, 0x41) => Some(66), // KEY_F8
        (0x07, 0x42) => Some(67), // KEY_F9
        (0x07, 0x43) => Some(68), // KEY_F10
        (0x07, 0x44) => Some(87), // KEY_F11
        (0x07, 0x45) => Some(88), // KEY_F12
        // Letters.
        (0x07, 0x04) => Some(30), // KEY_A
        (0x07, 0x05) => Some(48), // KEY_B
        (0x07, 0x06) => Some(46), // KEY_C
        (0x07, 0x07) => Some(32), // KEY_D
        (0x07, 0x08) => Some(18), // KEY_E
        (0x07, 0x09) => Some(33), // KEY_F
        (0x07, 0x0A) => Some(34), // KEY_G
        (0x07, 0x0B) => Some(35), // KEY_H
        (0x07, 0x0C) => Some(23), // KEY_I
        (0x07, 0x0D) => Some(36), // KEY_J
        (0x07, 0x0E) => Some(37), // KEY_K
        (0x07, 0x0F) => Some(38), // KEY_L
        (0x07, 0x10) => Some(50), // KEY_M
        (0x07, 0x11) => Some(49), // KEY_N
        (0x07, 0x12) => Some(24), // KEY_O
        (0x07, 0x13) => Some(25), // KEY_P
        (0x07, 0x14) => Some(16), // KEY_Q
        (0x07, 0x15) => Some(19), // KEY_R
        (0x07, 0x16) => Some(31), // KEY_S
        (0x07, 0x17) => Some(20), // KEY_T
        (0x07, 0x18) => Some(22), // KEY_U
        (0x07, 0x19) => Some(47), // KEY_V
        (0x07, 0x1A) => Some(17), // KEY_W
        (0x07, 0x1B) => Some(45), // KEY_X
        (0x07, 0x1C) => Some(21), // KEY_Y
        (0x07, 0x1D) => Some(44), // KEY_Z
        // Numbers.
        (0x07, 0x1E) => Some(2),  // KEY_1
        (0x07, 0x1F) => Some(3),  // KEY_2
        (0x07, 0x20) => Some(4),  // KEY_3
        (0x07, 0x21) => Some(5),  // KEY_4
        (0x07, 0x22) => Some(6),  // KEY_5
        (0x07, 0x23) => Some(7),  // KEY_6
        (0x07, 0x24) => Some(8),  // KEY_7
        (0x07, 0x25) => Some(9),  // KEY_8
        (0x07, 0x26) => Some(10), // KEY_9
        (0x07, 0x27) => Some(11), // KEY_0
        // Numpad.
        (0x07, 0x59) => Some(79),  // KEY_KP1
        (0x07, 0x5A) => Some(80),  // KEY_KP2
        (0x07, 0x5B) => Some(81),  // KEY_KP3
        (0x07, 0x5C) => Some(75),  // KEY_KP4
        (0x07, 0x5D) => Some(76),  // KEY_KP5
        (0x07, 0x5E) => Some(77),  // KEY_KP6
        (0x07, 0x5F) => Some(71),  // KEY_KP7
        (0x07, 0x60) => Some(72),  // KEY_KP8
        (0x07, 0x61) => Some(73),  // KEY_KP9
        (0x07, 0x62) => Some(82),  // KEY_KP0
        (0x07, 0x63) => Some(83),  // KEY_KPDOT
        (0x07, 0x55) => Some(55),  // KEY_KPASTERISK
        (0x07, 0x57) => Some(78),  // KEY_KPPLUS
        (0x07, 0x54) => Some(98),  // KEY_KPSLASH
        (0x07, 0x58) => Some(96),  // KEY_KPENTER
        (0x07, 0x56) => Some(74),  // KEY_KPMINUS
        (0x07, 0x65) => Some(140), // KEY_CALC
        (0x07, 0x67) => Some(117), // KEY_KPEQUAL
        // Punctuation / symbols.
        (0x07, 0x2D) => Some(12), // KEY_MINUS
        (0x07, 0x2E) => Some(13), // KEY_EQUAL
        (0x07, 0x2F) => Some(26), // KEY_LEFTBRACE
        (0x07, 0x31) => Some(27), // KEY_RIGHTBRACE
        (0x07, 0x30) => Some(43), // KEY_BACKSLASH
        (0x07, 0x33) => Some(39), // KEY_SEMICOLON
        (0x07, 0x34) => Some(40), // KEY_APOSTROPHE
        (0x07, 0x35) => Some(41), // KEY_GRAVE
        (0x07, 0x36) => Some(51), // KEY_COMMA
        (0x07, 0x37) => Some(53), // KEY_SLASH
        (0x07, 0x38) => Some(52), // KEY_DOT
        (0x07, 0x64) => Some(86), // KEY_102ND
        (0x07, 0x32) => Some(99), // KEY_HASHTILDE
        // --- Consumer page (0x0C) — media / display keys ---
        (0x0C, 0xCD) => Some(164), // KEY_PLAYPAUSE
        (0x0C, 0xE9) => Some(115), // KEY_VOLUMEUP
        (0x0C, 0xEA) => Some(114), // KEY_VOLUMEDOWN
        (0x0C, 0xE2) => Some(113), // KEY_MUTE
        (0x0C, 0xB5) => Some(163), // KEY_NEXTSONG
        (0x0C, 0xB6) => Some(165), // KEY_PREVIOUSSONG
        (0x0C, 0xB7) => Some(166), // KEY_STOPCD
        (0x0C, 0x6F) => Some(225), // KEY_BRIGHTNESSUP
        (0x0C, 0x70) => Some(224), // KEY_BRIGHTNESSDOWN
        _ => None,
    }
}

/// Map an evdev `KEY_*` code to its HID usage.
///
/// Input fallback for devices that do not emit `MSC_SCAN` (older kernels,
/// some virtual devices).  Covers the keyboard page subset plus the media
/// and display key codes the kernel derives from Consumer Page usages, so
/// that physical media keys can still be matched against rules.
pub fn keycode_to_hid_usage(code: u16) -> Option<HidUsage> {
    match code {
        // --- Keyboard page (0x07) ---
        29 => Some(HidUsage::LeftControl), // KEY_LEFTCTRL
        97 => Some(HidUsage::RightControl), // KEY_RIGHTCTRL
        42 => Some(HidUsage::LeftShift),   // KEY_LEFTSHIFT
        54 => Some(HidUsage::RightShift),  // KEY_RIGHTSHIFT
        56 => Some(HidUsage::LeftAlt),     // KEY_LEFTALT
        100 => Some(HidUsage::RightAlt),   // KEY_RIGHTALT
        125 => Some(HidUsage::LeftCommand), // KEY_LEFTMETA
        126 => Some(HidUsage::RightCommand), // KEY_RIGHTMETA
        58 => Some(HidUsage::CapsLock),    // KEY_CAPSLOCK
        15 => Some(HidUsage::Tab),         // KEY_TAB
        57 => Some(HidUsage::Space),       // KEY_SPACE
        28 => Some(HidUsage::Return),      // KEY_ENTER
        14 => Some(HidUsage::Backspace),   // KEY_BACKSPACE
        111 => Some(HidUsage::Delete),     // KEY_DELETE
        1 => Some(HidUsage::Escape),       // KEY_ESC
        103 => Some(HidUsage::UpArrow),    // KEY_UP
        108 => Some(HidUsage::DownArrow),  // KEY_DOWN
        105 => Some(HidUsage::LeftArrow),  // KEY_LEFT
        106 => Some(HidUsage::RightArrow), // KEY_RIGHT
        104 => Some(HidUsage::PageUp),     // KEY_PAGEUP
        109 => Some(HidUsage::PageDown),   // KEY_PAGEDOWN
        102 => Some(HidUsage::Home),       // KEY_HOME
        107 => Some(HidUsage::End),        // KEY_END
        59 => Some(HidUsage::F1),          // KEY_F1
        60 => Some(HidUsage::F2),          // KEY_F2
        61 => Some(HidUsage::F3),          // KEY_F3
        62 => Some(HidUsage::F4),          // KEY_F4
        63 => Some(HidUsage::F5),          // KEY_F5
        64 => Some(HidUsage::F6),          // KEY_F6
        65 => Some(HidUsage::F7),          // KEY_F7
        66 => Some(HidUsage::F8),          // KEY_F8
        67 => Some(HidUsage::F9),          // KEY_F9
        68 => Some(HidUsage::F10),         // KEY_F10
        87 => Some(HidUsage::F11),         // KEY_F11
        88 => Some(HidUsage::F12),         // KEY_F12
        30 => Some(HidUsage::A),           // KEY_A
        48 => Some(HidUsage::B),           // KEY_B
        46 => Some(HidUsage::C),           // KEY_C
        32 => Some(HidUsage::D),           // KEY_D
        18 => Some(HidUsage::E),           // KEY_E
        33 => Some(HidUsage::F),           // KEY_F
        34 => Some(HidUsage::G),           // KEY_G
        35 => Some(HidUsage::H),           // KEY_H
        23 => Some(HidUsage::I),           // KEY_I
        36 => Some(HidUsage::J),           // KEY_J
        37 => Some(HidUsage::K),           // KEY_K
        38 => Some(HidUsage::L),           // KEY_L
        50 => Some(HidUsage::M),           // KEY_M
        49 => Some(HidUsage::N),           // KEY_N
        24 => Some(HidUsage::O),           // KEY_O
        25 => Some(HidUsage::P),           // KEY_P
        16 => Some(HidUsage::Q),           // KEY_Q
        19 => Some(HidUsage::R),           // KEY_R
        31 => Some(HidUsage::S),           // KEY_S
        20 => Some(HidUsage::T),           // KEY_T
        22 => Some(HidUsage::U),           // KEY_U
        47 => Some(HidUsage::V),           // KEY_V
        17 => Some(HidUsage::W),           // KEY_W
        45 => Some(HidUsage::X),           // KEY_X
        21 => Some(HidUsage::Y),           // KEY_Y
        44 => Some(HidUsage::Z),           // KEY_Z
        2 => Some(HidUsage::Number1),      // KEY_1
        3 => Some(HidUsage::Number2),      // KEY_2
        4 => Some(HidUsage::Number3),      // KEY_3
        5 => Some(HidUsage::Number4),      // KEY_4
        6 => Some(HidUsage::Number5),      // KEY_5
        7 => Some(HidUsage::Number6),      // KEY_6
        8 => Some(HidUsage::Number7),      // KEY_7
        9 => Some(HidUsage::Number8),      // KEY_8
        10 => Some(HidUsage::Number9),     // KEY_9
        11 => Some(HidUsage::Number0),     // KEY_0
        79 => Some(HidUsage::Numpad1),     // KEY_KP1
        80 => Some(HidUsage::Numpad2),     // KEY_KP2
        81 => Some(HidUsage::Numpad3),     // KEY_KP3
        75 => Some(HidUsage::Numpad4),     // KEY_KP4
        76 => Some(HidUsage::Numpad5),     // KEY_KP5
        77 => Some(HidUsage::Numpad6),     // KEY_KP6
        71 => Some(HidUsage::Numpad7),     // KEY_KP7
        72 => Some(HidUsage::Numpad8),     // KEY_KP8
        73 => Some(HidUsage::Numpad9),     // KEY_KP9
        82 => Some(HidUsage::Numpad0),     // KEY_KP0
        83 => Some(HidUsage::NumpadDecimal), // KEY_KPDOT
        55 => Some(HidUsage::NumpadMultiply), // KEY_KPASTERISK
        78 => Some(HidUsage::NumpadPlus),  // KEY_KPPLUS
        98 => Some(HidUsage::NumpadDivide), // KEY_KPSLASH
        96 => Some(HidUsage::NumpadEnter), // KEY_KPENTER
        74 => Some(HidUsage::NumpadMinus), // KEY_KPMINUS
        140 => Some(HidUsage::NumpadClear), // KEY_CALC
        117 => Some(HidUsage::NumpadEqual), // KEY_KPEQUAL
        12 => Some(HidUsage::Minus),       // KEY_MINUS
        13 => Some(HidUsage::Equal),       // KEY_EQUAL
        26 => Some(HidUsage::BracketLeft), // KEY_LEFTBRACE
        27 => Some(HidUsage::BracketRight), // KEY_RIGHTBRACE
        43 => Some(HidUsage::Backslash),   // KEY_BACKSLASH
        39 => Some(HidUsage::Semicolon),   // KEY_SEMICOLON
        40 => Some(HidUsage::Quote),       // KEY_APOSTROPHE
        41 => Some(HidUsage::Grave),       // KEY_GRAVE
        51 => Some(HidUsage::Comma),       // KEY_COMMA
        53 => Some(HidUsage::Slash),       // KEY_SLASH
        52 => Some(HidUsage::Period),      // KEY_DOT
        86 => Some(HidUsage::IsoExtra),    // KEY_102ND
        99 => Some(HidUsage::IsoHash),     // KEY_HASHTILDE
        // --- Consumer page (0x0C) — media / display keys ---
        164 => Some(HidUsage::PlayPause), // KEY_PLAYPAUSE
        115 => Some(HidUsage::VolumeUp),  // KEY_VOLUMEUP
        114 => Some(HidUsage::VolumeDown), // KEY_VOLUMEDOWN
        113 => Some(HidUsage::Mute),      // KEY_MUTE
        163 => Some(HidUsage::NextTrack), // KEY_NEXTSONG
        165 => Some(HidUsage::PreviousTrack), // KEY_PREVIOUSSONG
        166 => Some(HidUsage::Stop),      // KEY_STOPCD
        225 => Some(HidUsage::BrightnessUp), // KEY_BRIGHTNESSUP
        224 => Some(HidUsage::BrightnessDown), // KEY_BRIGHTNESSDOWN
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_table_covers_all_known_usages() {
        // Every usage in HidUsage::ALL must have an evdev code so that no
        // compiled rule output can be dropped on emission.
        for usage in HidUsage::ALL {
            assert!(
                hid_usage_to_keycode(usage).is_some(),
                "missing evdev code for {}",
                usage.as_str()
            );
        }
    }

    #[test]
    fn tables_round_trip() {
        // Forward and reverse lookups must be exact inverses.
        for usage in HidUsage::ALL {
            let code = hid_usage_to_keycode(usage).expect("forward table");
            assert_eq!(
                keycode_to_hid_usage(code),
                Some(usage),
                "round-trip failed for {}",
                usage.as_str()
            );
        }
    }

    #[test]
    fn keyboard_page_values_match_kernel_header() {
        assert_eq!(
            hid_usage_to_keycode(HidUsage::A),
            Some(30) // KEY_A
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::Space),
            Some(57) // KEY_SPACE
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::LeftControl),
            Some(29) // KEY_LEFTCTRL
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::NumpadEnter),
            Some(96) // KEY_KPENTER
        );
    }

    #[test]
    fn consumer_page_values_match_kernel_header() {
        assert_eq!(
            hid_usage_to_keycode(HidUsage::PlayPause),
            Some(164) // KEY_PLAYPAUSE
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::VolumeUp),
            Some(115) // KEY_VOLUMEUP
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::VolumeDown),
            Some(114) // KEY_VOLUMEDOWN
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::Mute),
            Some(113) // KEY_MUTE
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::NextTrack),
            Some(163) // KEY_NEXTSONG
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::PreviousTrack),
            Some(165) // KEY_PREVIOUSSONG
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::Stop),
            Some(166) // KEY_STOPCD
        );
    }

    #[test]
    fn reverse_table_resolves_media_key_codes() {
        assert_eq!(keycode_to_hid_usage(164), Some(HidUsage::PlayPause));
        assert_eq!(keycode_to_hid_usage(115), Some(HidUsage::VolumeUp));
        assert_eq!(keycode_to_hid_usage(114), Some(HidUsage::VolumeDown));
        assert_eq!(keycode_to_hid_usage(113), Some(HidUsage::Mute));
    }

    #[test]
    fn unknown_codes_return_none() {
        assert_eq!(keycode_to_hid_usage(0), None);
        assert_eq!(keycode_to_hid_usage(0x2FF), None);
    }
}
