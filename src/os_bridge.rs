// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use crate::config::AbstractKey;

/// Cross-platform abstraction function translating the universal enum keys
/// to platform-specific system primitive codes at runtime compile-time check.
pub fn abstract_to_native_code(key: &AbstractKey) -> u32 {
    #[cfg(target_os = "windows")]
    {
        match key {
            AbstractKey::CapsLock => 0x14,    // VK_CAPITAL
            AbstractKey::LeftControl => 0xA2, // VK_LCONTROL
            AbstractKey::LeftAlt => 0xA4,     // VK_LMENU
            AbstractKey::Space => 0x20,       // VK_SPACE
            AbstractKey::F1 => 0x70,          // VK_F1
            AbstractKey::F2 => 0x71,          // VK_F2
            AbstractKey::LetterA => 0x41,     // 'A'
            AbstractKey::LetterT => 0x54,     // 'T'
        }
    }

    #[cfg(target_os = "macos")]
    {
        match key {
            AbstractKey::CapsLock => 57,
            AbstractKey::LeftControl => 59,
            AbstractKey::LeftAlt => 58, // Option
            AbstractKey::Space => 49,
            AbstractKey::F1 => 122,
            AbstractKey::F2 => 120,
            AbstractKey::LetterA => 0,
            AbstractKey::LetterT => 17,
        }
    }

    #[cfg(target_os = "linux")]
    {
        match key {
            AbstractKey::CapsLock => 58,    // KEY_CAPSLOCK
            AbstractKey::LeftControl => 29, // KEY_LEFTCTRL
            AbstractKey::LeftAlt => 56,     // KEY_LEFTALT
            AbstractKey::Space => 57,       // KEY_SPACE
            AbstractKey::F1 => 59,          // KEY_F1
            AbstractKey::F2 => 60,          // KEY_F2
            AbstractKey::LetterA => 30,     // KEY_A
            AbstractKey::LetterT => 20,     // KEY_T
        }
    }
}
