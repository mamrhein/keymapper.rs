// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The canonical 8-bit modifier bitmask layout.
//!
//! All platforms represent held modifiers as a `u8` mask whose bit positions
//! are the [`ModifierRole`] discriminants.  This module is the single source
//! of truth for the layout; the positions match the USB HID modifier byte,
//! so a role's HID modifier usage id (keyboard page) is its bit plus `0xE0`:
//!
//!   0 — Left Control    (HID 0xE0)
//!   1 — Left Shift      (HID 0xE1)
//!   2 — Left Alt        (HID 0xE2)
//!   3 — Left Command    (HID 0xE3; Win on Windows, Cmd on macOS)
//!   4 — Right Control   (HID 0xE4)
//!   5 — Right Shift     (HID 0xE5)
//!   6 — Right Alt       (HID 0xE6)
//!   7 — Right Command   (HID 0xE7)

/// Identifies a specific modifier key role.  The discriminant IS the bit
/// position in the universal 8-bit modifier mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ModifierRole {
    LeftControl = 0,
    LeftShift = 1,
    LeftAlt = 2,
    LeftCommand = 3,
    RightControl = 4,
    RightShift = 5,
    RightAlt = 6,
    RightCommand = 7,
}

impl ModifierRole {
    /// Returns the bit mask (power of two) for this modifier.  Used on
    /// Windows to build the modifier state from `GetAsyncKeyState`.
    #[allow(dead_code)]
    pub(crate) const fn mask(self) -> u8 {
        1u8 << self as u8
    }

    /// Try to create a `ModifierRole` from a bit position.  Returns `None`
    /// for values outside 0..8.
    pub(crate) const fn try_from_bit(bit: u8) -> Option<Self> {
        match bit {
            0 => Some(Self::LeftControl),
            1 => Some(Self::LeftShift),
            2 => Some(Self::LeftAlt),
            3 => Some(Self::LeftCommand),
            4 => Some(Self::RightControl),
            5 => Some(Self::RightShift),
            6 => Some(Self::RightAlt),
            7 => Some(Self::RightCommand),
            _ => None,
        }
    }

    /// Returns the HID modifier usage id (keyboard page) for this role.
    #[allow(dead_code)] // currently only used by linux-specific code
    pub(crate) const fn hid_id(self) -> u16 {
        0xE0 + self as u16
    }

    /// Maps a HID modifier usage id (keyboard page) to its role.  Returns
    /// `None` for ids outside the 0xE0–0xE7 range.
    pub(crate) const fn from_hid_id(id: u16) -> Option<Self> {
        match id {
            0xE0..=0xE7 => Self::try_from_bit(id as u8 - 0xE0),
            _ => None,
        }
    }
}
