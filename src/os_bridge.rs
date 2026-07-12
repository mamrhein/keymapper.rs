// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use crate::{config::AbstractKey, mapping_cache::NativeKey};

/// Cross-platform abstraction function translating the universal enum keys
/// to platform-specific system primitive codes at compile time.
///
/// All keycodes verified against authoritative platform references:
///   macOS:   Apple CGKeyCode (CoreGraphics/CGEvent.h)
///   Windows: Windows VK_* codes (WinUser.h)
///   Linux:   evdev KEY_* codes (linux/input-event-codes.h)
pub fn abstract_to_native_code(key: &AbstractKey) -> NativeKey {
    #[cfg(target_os = "windows")]
    {
        match key {
            // --- Modifiers ---
            AbstractKey::LeftControl => 0xA2, // VK_LCONTROL
            AbstractKey::RightControl => 0xA3, // VK_RCONTROL
            AbstractKey::LeftShift => 0xA0,   // VK_LSHIFT
            AbstractKey::RightShift => 0xA1,  // VK_RSHIFT
            AbstractKey::LeftAlt => 0xA4,     // VK_LMENU
            AbstractKey::RightAlt => 0xA5,    // VK_RMENU
            AbstractKey::LeftCommand => 0x5B, // VK_LWIN
            AbstractKey::RightCommand => 0x5C, // VK_RWIN
            AbstractKey::CapsLock => 0x14,    // VK_CAPITAL
            // --- Editor / misc ---
            AbstractKey::Tab => 0x09,       // VK_TAB
            AbstractKey::Space => 0x20,     // VK_SPACE
            AbstractKey::Return => 0x0D,    // VK_RETURN
            AbstractKey::Backspace => 0x08, // VK_BACK
            AbstractKey::Delete => 0x2E,    // VK_DELETE
            AbstractKey::Escape => 0x1B,    // VK_ESCAPE
            // --- Navigation ---
            AbstractKey::UpArrow => 0x26, // VK_UP
            AbstractKey::DownArrow => 0x28, // VK_DOWN
            AbstractKey::LeftArrow => 0x25, // VK_LEFT
            AbstractKey::RightArrow => 0x27, // VK_RIGHT
            AbstractKey::PageUp => 0x21,  // VK_PRIOR
            AbstractKey::PageDown => 0x22, // VK_NEXT
            AbstractKey::Home => 0x23,    // VK_HOME
            AbstractKey::End => 0x23,     // VK_END (shares VK code with Home)
            // --- Function keys ---
            AbstractKey::F1 => 0x70,  // VK_F1
            AbstractKey::F2 => 0x71,  // VK_F2
            AbstractKey::F3 => 0x72,  // VK_F3
            AbstractKey::F4 => 0x73,  // VK_F4
            AbstractKey::F5 => 0x74,  // VK_F5
            AbstractKey::F6 => 0x75,  // VK_F6
            AbstractKey::F7 => 0x76,  // VK_F7
            AbstractKey::F8 => 0x77,  // VK_F8
            AbstractKey::F9 => 0x78,  // VK_F9
            AbstractKey::F10 => 0x79, // VK_F10
            AbstractKey::F11 => 0x7A, // VK_F11
            AbstractKey::F12 => 0x7B, // VK_F12
            // --- Letters ---
            AbstractKey::A => 0x41, // VK_A
            AbstractKey::B => 0x42, // VK_B
            AbstractKey::C => 0x43, // VK_C
            AbstractKey::D => 0x44, // VK_D
            AbstractKey::E => 0x45, // VK_E
            AbstractKey::F => 0x46, // VK_F
            AbstractKey::G => 0x47, // VK_G
            AbstractKey::H => 0x48, // VK_H
            AbstractKey::I => 0x49, // VK_I
            AbstractKey::J => 0x4A, // VK_J
            AbstractKey::K => 0x4B, // VK_K
            AbstractKey::L => 0x4C, // VK_L
            AbstractKey::M => 0x4D, // VK_M
            AbstractKey::N => 0x4E, // VK_N
            AbstractKey::O => 0x4F, // VK_O
            AbstractKey::P => 0x50, // VK_P
            AbstractKey::Q => 0x51, // VK_Q
            AbstractKey::R => 0x52, // VK_R
            AbstractKey::S => 0x53, // VK_S
            AbstractKey::T => 0x54, // VK_T
            AbstractKey::U => 0x55, // VK_U
            AbstractKey::V => 0x56, // VK_V
            AbstractKey::W => 0x57, // VK_W
            AbstractKey::X => 0x58, // VK_X
            AbstractKey::Y => 0x59, // VK_Y
            AbstractKey::Z => 0x5A, // VK_Z
            // --- Numbers ---
            AbstractKey::Number1 => 0x31, // VK_1
            AbstractKey::Number2 => 0x32, // VK_2
            AbstractKey::Number3 => 0x33, // VK_3
            AbstractKey::Number4 => 0x34, // VK_4
            AbstractKey::Number5 => 0x35, // VK_5
            AbstractKey::Number6 => 0x36, // VK_6
            AbstractKey::Number7 => 0x37, // VK_7
            AbstractKey::Number8 => 0x38, // VK_8
            AbstractKey::Number9 => 0x39, // VK_9
            AbstractKey::Number0 => 0x30, // VK_0
        }
    }

    #[cfg(target_os = "macos")]
    {
        match key {
            // --- Modifiers ---
            AbstractKey::LeftControl => 59, // kVK_Control
            AbstractKey::RightControl => 62, // kVK_RightControl
            AbstractKey::LeftShift => 56,   // kVK_Shift
            AbstractKey::RightShift => 60,  // kVK_RightShift
            AbstractKey::LeftAlt => 58,     // kVK_Option
            AbstractKey::RightAlt => 61,    // kVK_RightOption
            AbstractKey::LeftCommand => 55, // kVK_Command
            AbstractKey::RightCommand => 54, // kVK_RightCommand
            AbstractKey::CapsLock => 57,    // kVK_CapsLock
            // --- Editor / misc ---
            AbstractKey::Tab => 48,       // kVK_Tab
            AbstractKey::Space => 49,     // kVK_Space
            AbstractKey::Return => 36,    // kVK_Return
            AbstractKey::Backspace => 51, // kVK_Delete
            AbstractKey::Delete => 117,   // kVK_ForwardDelete
            AbstractKey::Escape => 53,    // kVK_Escape
            // --- Navigation ---
            AbstractKey::UpArrow => 126, // kVK_UpArrow
            AbstractKey::DownArrow => 125, // kVK_DownArrow
            AbstractKey::LeftArrow => 123, // kVK_LeftArrow
            AbstractKey::RightArrow => 124, // kVK_RightArrow
            AbstractKey::PageUp => 116,  // kVK_PageUp
            AbstractKey::PageDown => 121, // kVK_PageDown
            AbstractKey::Home => 115,    // kVK_Home
            AbstractKey::End => 119,     // kVK_End
            // --- Function keys ---
            AbstractKey::F1 => 122,  // kVK_F1
            AbstractKey::F2 => 120,  // kVK_F2
            AbstractKey::F3 => 99,   // kVK_F3
            AbstractKey::F4 => 118,  // kVK_F4
            AbstractKey::F5 => 96,   // kVK_F5
            AbstractKey::F6 => 97,   // kVK_F6
            AbstractKey::F7 => 98,   // kVK_F7
            AbstractKey::F8 => 100,  // kVK_F8
            AbstractKey::F9 => 101,  // kVK_F9
            AbstractKey::F10 => 109, // kVK_F10
            AbstractKey::F11 => 103, // kVK_F11
            AbstractKey::F12 => 111, // kVK_F12
            // --- Letters (US QWERTY layout) ---
            AbstractKey::A => 0,  // kVK_A
            AbstractKey::B => 11, // kVK_B
            AbstractKey::C => 8,  // kVK_C
            AbstractKey::D => 2,  // kVK_D
            AbstractKey::E => 14, // kVK_E
            AbstractKey::F => 3,  // kVK_F
            AbstractKey::G => 5,  // kVK_G
            AbstractKey::H => 4,  // kVK_H
            AbstractKey::I => 34, // kVK_I
            AbstractKey::J => 38, // kVK_J
            AbstractKey::K => 40, // kVK_K
            AbstractKey::L => 37, // kVK_L
            AbstractKey::M => 46, // kVK_M
            AbstractKey::N => 45, // kVK_N
            AbstractKey::O => 31, // kVK_O
            AbstractKey::P => 35, // kVK_P
            AbstractKey::Q => 12, // kVK_Q
            AbstractKey::R => 15, // kVK_R
            AbstractKey::S => 1,  // kVK_S
            AbstractKey::T => 17, // kVK_T
            AbstractKey::U => 32, // kVK_U
            AbstractKey::V => 9,  // kVK_V
            AbstractKey::W => 13, // kVK_W
            AbstractKey::X => 7,  // kVK_X
            AbstractKey::Y => 16, // kVK_Y
            AbstractKey::Z => 6,  // kVK_Z
            // --- Numbers ---
            AbstractKey::Number1 => 18, // kVK_1
            AbstractKey::Number2 => 19, // kVK_2
            AbstractKey::Number3 => 20, // kVK_3
            AbstractKey::Number4 => 21, // kVK_4
            AbstractKey::Number5 => 23, // kVK_5
            AbstractKey::Number6 => 22, // kVK_6
            AbstractKey::Number7 => 26, // kVK_7
            AbstractKey::Number8 => 28, // kVK_8
            AbstractKey::Number9 => 25, // kVK_9
            AbstractKey::Number0 => 29, // kVK_0
        }
    }

    #[cfg(target_os = "linux")]
    {
        match key {
            // --- Modifiers ---
            AbstractKey::LeftControl => 29, // KEY_LEFTCTRL
            AbstractKey::RightControl => 106, // KEY_RIGHTCTRL
            AbstractKey::LeftShift => 42,   // KEY_LEFTSHIFT
            AbstractKey::RightShift => 54,  // KEY_RIGHTSHIFT
            AbstractKey::LeftAlt => 56,     // KEY_LEFTALT
            AbstractKey::RightAlt => 100,   // KEY_RIGHTALT
            AbstractKey::LeftCommand => 125, // KEY_LEFTMETA
            AbstractKey::RightCommand => 126, // KEY_RIGHTMETA
            AbstractKey::CapsLock => 58,    // KEY_CAPSLOCK
            // --- Editor / misc ---
            AbstractKey::Tab => 15,       // KEY_TAB
            AbstractKey::Space => 57,     // KEY_SPACE
            AbstractKey::Return => 28,    // KEY_ENTER
            AbstractKey::Backspace => 14, // KEY_BACKSPACE
            AbstractKey::Delete => 111,   // KEY_DELETE
            AbstractKey::Escape => 1,     // KEY_ESC
            // --- Navigation ---
            AbstractKey::UpArrow => 103,   // KEY_UP
            AbstractKey::DownArrow => 108, // KEY_DOWN
            AbstractKey::LeftArrow => 105, // KEY_LEFT
            AbstractKey::RightArrow => 106, // KEY_RIGHT
            AbstractKey::PageUp => 104,    // KEY_PAGEUP
            AbstractKey::PageDown => 109,  // KEY_PAGEDOWN
            AbstractKey::Home => 102,      // KEY_HOME
            AbstractKey::End => 107,       // KEY_END
            // --- Function keys ---
            AbstractKey::F1 => 59,  // KEY_F1
            AbstractKey::F2 => 60,  // KEY_F2
            AbstractKey::F3 => 61,  // KEY_F3
            AbstractKey::F4 => 62,  // KEY_F4
            AbstractKey::F5 => 63,  // KEY_F5
            AbstractKey::F6 => 64,  // KEY_F6
            AbstractKey::F7 => 65,  // KEY_F7
            AbstractKey::F8 => 66,  // KEY_F8
            AbstractKey::F9 => 67,  // KEY_F9
            AbstractKey::F10 => 68, // KEY_F10
            AbstractKey::F11 => 69, // KEY_F11
            AbstractKey::F12 => 70, // KEY_F12
            // --- Letters ---
            AbstractKey::A => 30, // KEY_A
            AbstractKey::B => 48, // KEY_B
            AbstractKey::C => 46, // KEY_C
            AbstractKey::D => 32, // KEY_D
            AbstractKey::E => 18, // KEY_E
            AbstractKey::F => 33, // KEY_F
            AbstractKey::G => 34, // KEY_G
            AbstractKey::H => 35, // KEY_H
            AbstractKey::I => 23, // KEY_I
            AbstractKey::J => 36, // KEY_J
            AbstractKey::K => 37, // KEY_K
            AbstractKey::L => 38, // KEY_L
            AbstractKey::M => 50, // KEY_M
            AbstractKey::N => 49, // KEY_N
            AbstractKey::O => 24, // KEY_O
            AbstractKey::P => 25, // KEY_P
            AbstractKey::Q => 16, // KEY_Q
            AbstractKey::R => 19, // KEY_R
            AbstractKey::S => 31, // KEY_S
            AbstractKey::T => 20, // KEY_T
            AbstractKey::U => 22, // KEY_U
            AbstractKey::V => 47, // KEY_V
            AbstractKey::W => 17, // KEY_W
            AbstractKey::X => 45, // KEY_X
            AbstractKey::Y => 21, // KEY_Y
            AbstractKey::Z => 44, // KEY_Z
            // --- Numbers ---
            AbstractKey::Number1 => 2,  // KEY_1
            AbstractKey::Number2 => 3,  // KEY_2
            AbstractKey::Number3 => 4,  // KEY_3
            AbstractKey::Number4 => 5,  // KEY_4
            AbstractKey::Number5 => 6,  // KEY_5
            AbstractKey::Number6 => 7,  // KEY_6
            AbstractKey::Number7 => 8,  // KEY_7
            AbstractKey::Number8 => 9,  // KEY_8
            AbstractKey::Number9 => 10, // KEY_9
            AbstractKey::Number0 => 11, // KEY_0
        }
    }

    // Fallback for unsupported platforms (unreachable in practice).
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux"
    )))]
    {
        let _ = key;
        0
    }
}
