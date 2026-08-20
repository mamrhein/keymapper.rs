// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Shared modifier bit layout.
//!
//! All platforms use the same 8-bit modifier mask where each bit corresponds
//! to a specific physical modifier key.  This module defines the canonical
//! mapping so platforms don't duplicate magic numbers.
//!
//! Bit positions:
//!   0 — Left Control
//!   1 — Right Control
//!   2 — Left Shift
//!   3 — Right Shift
//!   4 — Left Alt
//!   5 — Right Alt
//!   6 — Left Command (Win on Windows, Cmd on macOS)
//!   7 — Right Command

/// Identifies a specific modifier key role.  The discriminant IS the bit
/// position in the universal 8-bit modifier mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ModifierRole {
    LeftControl = 0,
    RightControl = 1,
    LeftShift = 2,
    RightShift = 3,
    LeftAlt = 4,
    RightAlt = 5,
    LeftCommand = 6,
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
    #[allow(dead_code)]
    pub(crate) const fn try_from_bit(bit: u8) -> Option<Self> {
        match bit {
            0 => Some(Self::LeftControl),
            1 => Some(Self::RightControl),
            2 => Some(Self::LeftShift),
            3 => Some(Self::RightShift),
            4 => Some(Self::LeftAlt),
            5 => Some(Self::RightAlt),
            6 => Some(Self::LeftCommand),
            7 => Some(Self::RightCommand),
            _ => None,
        }
    }
}
