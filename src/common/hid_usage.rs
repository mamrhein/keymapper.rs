// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! HID-centric key identities.
//!
//! This enum represents the logical identity of a physical key using USB HID
//! Usage Tables codes.  Each variant's discriminant encodes the combined HID
//! usage as `(page << 16) | id`, enabling direct conversion from Linux
//! MSC_SCAN input events and other HID-based sources.
//!
//! The enum contains variants from two HID usage pages:
//! - Keyboard/Keypad page (0x07) — standard keyboard keys.
//! - Consumer page (0x0C) — media and display control keys.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::common::modifier::ModifierRole;

/// HID usage page for Keyboard/Keypad.
pub const PAGE_KEYBOARD: u16 = 0x07;

/// HID usage page for Consumer.
pub const PAGE_CONSUMER: u16 = 0x0C;

/// HID-based key identity for configuration and cross-platform code.
///
/// The discriminant encodes the combined HID usage as `(page << 16) | id`.
/// This matches the format of Linux MSC_SCAN codes, allowing direct
/// conversion via `from_code()`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HidUsage {
    // --- Keyboard page (0x07) — Modifiers ---
    LeftControl = 0x0700E0,
    LeftShift = 0x0700E1,
    LeftAlt = 0x0700E2,
    LeftCommand = 0x0700E3,
    RightControl = 0x0700E4,
    RightShift = 0x0700E5,
    RightAlt = 0x0700E6,
    RightCommand = 0x0700E7,
    // --- Keyboard page — Caps Lock ---
    CapsLock = 0x070039,
    // --- Keyboard page — Editor / misc ---
    Return = 0x070028,
    Escape = 0x070029,
    Backspace = 0x07002A,
    Tab = 0x07002B,
    Space = 0x07002C,
    // Insert = 0x070049
    Delete = 0x07004C,
    // --- Keyboard page — Navigation ---
    Home = 0x07004A,
    PageUp = 0x07004B,
    End = 0x07004D,
    PageDown = 0x07004E,
    RightArrow = 0x07004F,
    LeftArrow = 0x070050,
    DownArrow = 0x070051,
    UpArrow = 0x070052,
    // --- Keyboard page — Function keys ---
    F1 = 0x07003A,
    F2 = 0x07003B,
    F3 = 0x07003C,
    F4 = 0x07003D,
    F5 = 0x07003E,
    F6 = 0x07003F,
    F7 = 0x070040,
    F8 = 0x070041,
    F9 = 0x070042,
    F10 = 0x070043,
    F11 = 0x070044,
    F12 = 0x070045,
    // PrintScreen = 0x070046
    // ScrollLock = 0x070047
    // Pause = 0x070048
    // --- Keyboard page — Letters ---
    A = 0x070004,
    B = 0x070005,
    C = 0x070006,
    D = 0x070007,
    E = 0x070008,
    F = 0x070009,
    G = 0x07000A,
    H = 0x07000B,
    I = 0x07000C,
    J = 0x07000D,
    K = 0x07000E,
    L = 0x07000F,
    M = 0x070010,
    N = 0x070011,
    O = 0x070012,
    P = 0x070013,
    Q = 0x070014,
    R = 0x070015,
    S = 0x070016,
    T = 0x070017,
    U = 0x070018,
    V = 0x070019,
    W = 0x07001A,
    X = 0x07001B,
    Y = 0x07001C,
    Z = 0x07001D,
    // --- Keyboard page — Numbers ---
    Number1 = 0x07001E,
    Number2 = 0x07001F,
    Number3 = 0x070020,
    Number4 = 0x070021,
    Number5 = 0x070022,
    Number6 = 0x070023,
    Number7 = 0x070024,
    Number8 = 0x070025,
    Number9 = 0x070026,
    Number0 = 0x070027,
    // --- Keyboard page — Numpad ---
    Numpad1 = 0x070059,
    Numpad2 = 0x07005A,
    Numpad3 = 0x07005B,
    Numpad4 = 0x07005C,
    Numpad5 = 0x07005D,
    Numpad6 = 0x07005E,
    Numpad7 = 0x07005F,
    Numpad8 = 0x070060,
    Numpad9 = 0x070061,
    Numpad0 = 0x070062,
    NumpadDecimal = 0x070063,
    NumpadMultiply = 0x070055,
    NumpadPlus = 0x070057,
    NumpadDivide = 0x070054,
    NumpadEnter = 0x070058,
    NumpadMinus = 0x070056,
    NumpadClear = 0x070065,
    NumpadEqual = 0x070067,
    // --- Keyboard page — Punctuation / symbols ---
    Minus = 0x07002D,
    Equal = 0x07002E,
    BracketLeft = 0x07002F,
    BracketRight = 0x070031,
    Backslash = 0x070030,
    Semicolon = 0x070033,
    Quote = 0x070034,
    Grave = 0x070035,
    Comma = 0x070036,
    Slash = 0x070037,
    Period = 0x070038,
    IsoExtra = 0x070064,
    IsoHash = 0x070032,
    // --- Consumer page (0x0C) — Media controls ---
    PlayPause = 0x0C00CD,
    VolumeUp = 0x0C00E9,
    VolumeDown = 0x0C00EA,
    Mute = 0x0C00E2,
    NextTrack = 0x0C00B5,
    PreviousTrack = 0x0C00B6,
    Stop = 0x0C00B7,
    // --- Consumer page — Display controls ---
    BrightnessUp = 0x0C006F,
    BrightnessDown = 0x0C0070,
}

impl HidUsage {
    // -------------------------------------------------------------------
    // Code accessors — combined (page << 16) | id
    // -------------------------------------------------------------------

    /// Return the combined HID usage code `(page << 16) | id`.
    ///
    /// This matches the format of Linux MSC_SCAN codes, so a Linux scan
    /// code can be passed directly to `from_code()` to obtain the
    /// corresponding `HidUsage`.
    #[inline]
    pub const fn code(self) -> u32 {
        self as u32
    }

    /// Construct a `HidUsage` from a combined HID usage code.
    ///
    /// Returns `None` if the code does not match a recognized usage.
    #[inline]
    pub fn from_code(code: u32) -> Option<Self> {
        match code {
            0x0700E0 => Some(Self::LeftControl),
            0x0700E1 => Some(Self::LeftShift),
            0x0700E2 => Some(Self::LeftAlt),
            0x0700E3 => Some(Self::LeftCommand),
            0x0700E4 => Some(Self::RightControl),
            0x0700E5 => Some(Self::RightShift),
            0x0700E6 => Some(Self::RightAlt),
            0x0700E7 => Some(Self::RightCommand),
            0x070039 => Some(Self::CapsLock),
            0x070028 => Some(Self::Return),
            0x070029 => Some(Self::Escape),
            0x07002A => Some(Self::Backspace),
            0x07002B => Some(Self::Tab),
            0x07002C => Some(Self::Space),
            0x07004C => Some(Self::Delete),
            0x07004A => Some(Self::Home),
            0x07004B => Some(Self::PageUp),
            0x07004D => Some(Self::End),
            0x07004E => Some(Self::PageDown),
            0x07004F => Some(Self::RightArrow),
            0x070050 => Some(Self::LeftArrow),
            0x070051 => Some(Self::DownArrow),
            0x070052 => Some(Self::UpArrow),
            0x07003A => Some(Self::F1),
            0x07003B => Some(Self::F2),
            0x07003C => Some(Self::F3),
            0x07003D => Some(Self::F4),
            0x07003E => Some(Self::F5),
            0x07003F => Some(Self::F6),
            0x070040 => Some(Self::F7),
            0x070041 => Some(Self::F8),
            0x070042 => Some(Self::F9),
            0x070043 => Some(Self::F10),
            0x070044 => Some(Self::F11),
            0x070045 => Some(Self::F12),
            0x070004 => Some(Self::A),
            0x070005 => Some(Self::B),
            0x070006 => Some(Self::C),
            0x070007 => Some(Self::D),
            0x070008 => Some(Self::E),
            0x070009 => Some(Self::F),
            0x07000A => Some(Self::G),
            0x07000B => Some(Self::H),
            0x07000C => Some(Self::I),
            0x07000D => Some(Self::J),
            0x07000E => Some(Self::K),
            0x07000F => Some(Self::L),
            0x070010 => Some(Self::M),
            0x070011 => Some(Self::N),
            0x070012 => Some(Self::O),
            0x070013 => Some(Self::P),
            0x070014 => Some(Self::Q),
            0x070015 => Some(Self::R),
            0x070016 => Some(Self::S),
            0x070017 => Some(Self::T),
            0x070018 => Some(Self::U),
            0x070019 => Some(Self::V),
            0x07001A => Some(Self::W),
            0x07001B => Some(Self::X),
            0x07001C => Some(Self::Y),
            0x07001D => Some(Self::Z),
            0x07001E => Some(Self::Number1),
            0x07001F => Some(Self::Number2),
            0x070020 => Some(Self::Number3),
            0x070021 => Some(Self::Number4),
            0x070022 => Some(Self::Number5),
            0x070023 => Some(Self::Number6),
            0x070024 => Some(Self::Number7),
            0x070025 => Some(Self::Number8),
            0x070026 => Some(Self::Number9),
            0x070027 => Some(Self::Number0),
            0x070062 => Some(Self::Numpad0),
            0x070059 => Some(Self::Numpad1),
            0x07005A => Some(Self::Numpad2),
            0x07005B => Some(Self::Numpad3),
            0x07005C => Some(Self::Numpad4),
            0x07005D => Some(Self::Numpad5),
            0x07005E => Some(Self::Numpad6),
            0x07005F => Some(Self::Numpad7),
            0x070060 => Some(Self::Numpad8),
            0x070061 => Some(Self::Numpad9),
            0x070063 => Some(Self::NumpadDecimal),
            0x070055 => Some(Self::NumpadMultiply),
            0x070057 => Some(Self::NumpadPlus),
            0x070054 => Some(Self::NumpadDivide),
            0x070058 => Some(Self::NumpadEnter),
            0x070056 => Some(Self::NumpadMinus),
            0x070065 => Some(Self::NumpadClear),
            0x070067 => Some(Self::NumpadEqual),
            0x07002D => Some(Self::Minus),
            0x07002E => Some(Self::Equal),
            0x07002F => Some(Self::BracketLeft),
            0x070031 => Some(Self::BracketRight),
            0x070030 => Some(Self::Backslash),
            0x070033 => Some(Self::Semicolon),
            0x070034 => Some(Self::Quote),
            0x070035 => Some(Self::Grave),
            0x070036 => Some(Self::Comma),
            0x070037 => Some(Self::Slash),
            0x070038 => Some(Self::Period),
            0x070064 => Some(Self::IsoExtra),
            0x070032 => Some(Self::IsoHash),
            // Consumer page
            0x0C00CD => Some(Self::PlayPause),
            0x0C00E9 => Some(Self::VolumeUp),
            0x0C00EA => Some(Self::VolumeDown),
            0x0C00E2 => Some(Self::Mute),
            0x0C00B5 => Some(Self::NextTrack),
            0x0C00B6 => Some(Self::PreviousTrack),
            0x0C00B7 => Some(Self::Stop),
            0x0C006F => Some(Self::BrightnessUp),
            0x0C0070 => Some(Self::BrightnessDown),
            _ => None,
        }
    }

    // -------------------------------------------------------------------
    // Page and id accessors
    // -------------------------------------------------------------------

    /// Return the HID usage page.
    #[inline]
    pub const fn page(self) -> u16 {
        (self as u32 >> 16) as u16
    }

    /// Return the HID usage id within its page.
    #[inline]
    pub const fn id(self) -> u16 {
        (self as u32 & 0xFFFF) as u16
    }

    // -------------------------------------------------------------------
    // Convenience constructors — build a code and look it up
    // -------------------------------------------------------------------

    /// Construct a `HidUsage` from a Keyboard/Keypad page usage id.
    ///
    /// Returns `None` if the id is not a recognized keyboard usage.
    pub fn keyboard(id: u16) -> Option<Self> {
        Self::from_code(((PAGE_KEYBOARD as u32) << 16) | id as u32)
    }

    /// Construct a `HidUsage` from a Consumer page usage id.
    ///
    /// Returns `None` if the id is not a recognized consumer usage.
    pub fn consumer(id: u16) -> Option<Self> {
        Self::from_code(((PAGE_CONSUMER as u32) << 16) | id as u32)
    }

    // -------------------------------------------------------------------
    // String conversion (canonical config name)
    // -------------------------------------------------------------------

    /// Return the canonical config-name for this key.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LeftControl => "LeftControl",
            Self::RightControl => "RightControl",
            Self::LeftShift => "LeftShift",
            Self::RightShift => "RightShift",
            Self::LeftAlt => "LeftAlt",
            Self::RightAlt => "RightAlt",
            Self::LeftCommand => "LeftCommand",
            Self::RightCommand => "RightCommand",
            Self::CapsLock => "CapsLock",
            Self::Tab => "Tab",
            Self::Space => "Space",
            Self::Return => "Return",
            Self::Backspace => "Backspace",
            Self::Delete => "Delete",
            Self::Escape => "Escape",
            Self::UpArrow => "UpArrow",
            Self::DownArrow => "DownArrow",
            Self::LeftArrow => "LeftArrow",
            Self::RightArrow => "RightArrow",
            Self::PageUp => "PageUp",
            Self::PageDown => "PageDown",
            Self::Home => "Home",
            Self::End => "End",
            Self::F1 => "F1",
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
            Self::F5 => "F5",
            Self::F6 => "F6",
            Self::F7 => "F7",
            Self::F8 => "F8",
            Self::F9 => "F9",
            Self::F10 => "F10",
            Self::F11 => "F11",
            Self::F12 => "F12",
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::E => "E",
            Self::F => "F",
            Self::G => "G",
            Self::H => "H",
            Self::I => "I",
            Self::J => "J",
            Self::K => "K",
            Self::L => "L",
            Self::M => "M",
            Self::N => "N",
            Self::O => "O",
            Self::P => "P",
            Self::Q => "Q",
            Self::R => "R",
            Self::S => "S",
            Self::T => "T",
            Self::U => "U",
            Self::V => "V",
            Self::W => "W",
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
            Self::Number1 => "1",
            Self::Number2 => "2",
            Self::Number3 => "3",
            Self::Number4 => "4",
            Self::Number5 => "5",
            Self::Number6 => "6",
            Self::Number7 => "7",
            Self::Number8 => "8",
            Self::Number9 => "9",
            Self::Number0 => "0",
            // Numpad
            Self::Numpad0 => "Numpad0",
            Self::Numpad1 => "Numpad1",
            Self::Numpad2 => "Numpad2",
            Self::Numpad3 => "Numpad3",
            Self::Numpad4 => "Numpad4",
            Self::Numpad5 => "Numpad5",
            Self::Numpad6 => "Numpad6",
            Self::Numpad7 => "Numpad7",
            Self::Numpad8 => "Numpad8",
            Self::Numpad9 => "Numpad9",
            Self::NumpadDecimal => "NumpadDecimal",
            Self::NumpadMultiply => "NumpadMultiply",
            Self::NumpadPlus => "NumpadPlus",
            Self::NumpadDivide => "NumpadDivide",
            Self::NumpadEnter => "NumpadEnter",
            Self::NumpadMinus => "NumpadMinus",
            Self::NumpadClear => "NumpadClear",
            Self::NumpadEqual => "NumpadEqual",
            // Punctuation / symbols
            Self::Minus => "Minus",
            Self::Equal => "Equal",
            Self::BracketLeft => "BracketLeft",
            Self::BracketRight => "BracketRight",
            Self::Backslash => "Backslash",
            Self::Semicolon => "Semicolon",
            Self::Quote => "Quote",
            Self::Comma => "Comma",
            Self::Period => "Period",
            Self::Slash => "Slash",
            Self::Grave => "Grave",
            Self::IsoExtra => "IsoExtra",
            Self::IsoHash => "IsoHash",
            // Consumer page — media controls
            Self::PlayPause => "PlayPause",
            Self::VolumeUp => "VolumeUp",
            Self::VolumeDown => "VolumeDown",
            Self::Mute => "Mute",
            Self::NextTrack => "NextTrack",
            Self::PreviousTrack => "PreviousTrack",
            Self::Stop => "Stop",
            // Consumer page — display controls
            Self::BrightnessUp => "BrightnessUp",
            Self::BrightnessDown => "BrightnessDown",
        }
    }

    // -------------------------------------------------------------------
    // Enumeration
    // -------------------------------------------------------------------

    /// All defined `HidUsage` variants.
    ///
    /// Contains 111 entries: 102 keyboard/keypad usages plus 9 consumer
    /// page usages.
    pub fn all() -> &'static [Self] {
        &Self::ALL
    }

    /// Array of all defined `HidUsage` variants (111 total).
    pub const ALL: [Self; 111] = [
        // Modifiers
        Self::LeftControl,
        Self::RightControl,
        Self::LeftShift,
        Self::RightShift,
        Self::LeftAlt,
        Self::RightAlt,
        Self::LeftCommand,
        Self::RightCommand,
        // Caps Lock
        Self::CapsLock,
        // Editor / misc
        Self::Tab,
        Self::Space,
        Self::Return,
        Self::Backspace,
        Self::Delete,
        Self::Escape,
        // Navigation
        Self::UpArrow,
        Self::DownArrow,
        Self::LeftArrow,
        Self::RightArrow,
        Self::PageUp,
        Self::PageDown,
        Self::Home,
        Self::End,
        // Function keys
        Self::F1,
        Self::F2,
        Self::F3,
        Self::F4,
        Self::F5,
        Self::F6,
        Self::F7,
        Self::F8,
        Self::F9,
        Self::F10,
        Self::F11,
        Self::F12,
        // Letters
        Self::A,
        Self::B,
        Self::C,
        Self::D,
        Self::E,
        Self::F,
        Self::G,
        Self::H,
        Self::I,
        Self::J,
        Self::K,
        Self::L,
        Self::M,
        Self::N,
        Self::O,
        Self::P,
        Self::Q,
        Self::R,
        Self::S,
        Self::T,
        Self::U,
        Self::V,
        Self::W,
        Self::X,
        Self::Y,
        Self::Z,
        // Numbers
        Self::Number1,
        Self::Number2,
        Self::Number3,
        Self::Number4,
        Self::Number5,
        Self::Number6,
        Self::Number7,
        Self::Number8,
        Self::Number9,
        Self::Number0,
        // Numpad
        Self::Numpad0,
        Self::Numpad1,
        Self::Numpad2,
        Self::Numpad3,
        Self::Numpad4,
        Self::Numpad5,
        Self::Numpad6,
        Self::Numpad7,
        Self::Numpad8,
        Self::Numpad9,
        Self::NumpadDecimal,
        Self::NumpadMultiply,
        Self::NumpadPlus,
        Self::NumpadDivide,
        Self::NumpadEnter,
        Self::NumpadMinus,
        Self::NumpadClear,
        Self::NumpadEqual,
        // Punctuation / symbols
        Self::Minus,
        Self::Equal,
        Self::BracketLeft,
        Self::BracketRight,
        Self::Backslash,
        Self::Semicolon,
        Self::Quote,
        Self::Grave,
        Self::Comma,
        Self::Slash,
        Self::Period,
        Self::IsoExtra,
        Self::IsoHash,
        // Consumer page — media controls
        Self::PlayPause,
        Self::VolumeUp,
        Self::VolumeDown,
        Self::Mute,
        Self::NextTrack,
        Self::PreviousTrack,
        Self::Stop,
        // Consumer page — display controls
        Self::BrightnessUp,
        Self::BrightnessDown,
    ];

    // -------------------------------------------------------------------
    // Modifier mapping
    // -------------------------------------------------------------------

    /// Map a modifier HID usage to the unified modifier bit position.
    ///
    /// Returns `None` if the usage is not a recognised modifier key.  This
    /// replaces all per-platform `keycode_to_modifier_bit()` functions with a
    /// single canonical mapping; the bit layout itself is defined by
    /// `common::modifier::ModifierRole`.
    pub fn hid_usage_to_modifier_bit(usage: Self) -> Option<u8> {
        if usage.page() != PAGE_KEYBOARD {
            return None;
        }
        ModifierRole::from_hid_id(usage.id()).map(|role| role as u8)
    }
}

impl fmt::Display for HidUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Serialize for HidUsage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HidUsage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::try_from(s.as_str())
            .map_err(|e| serde::de::Error::custom(e.to_string()))
    }
}

/// Returns a user-friendly error message for an unrecognised key name.
///
/// Shared by the `HidUsage` parser and the platform `Key` enums, which all
/// resolve the same set of config-facing key names.
pub(crate) fn unknown_key_error(s: &str) -> String {
    format!(
        "Unknown key name '{}'. Use names like CapsLock, LeftCtrl, A, F1, 1, \
         Minus, Equal, BracketLeft, etc.",
        s
    )
}

/// Error returned when a string cannot be parsed as a `HidUsage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidUsageParseError(pub String);

impl fmt::Display for HidUsageParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unknown_key_error(&self.0))
    }
}

impl std::error::Error for HidUsageParseError {}

/// Parse a `HidUsage` from a string slice.
///
/// Accepts canonical names (`LeftControl`, `A`, `F1`) and common aliases
/// (`Ctrl`, `Cmd`, `Esc`).  Generic modifier names resolve to left-side
/// defaults.  Case-sensitive.
impl TryFrom<&str> for HidUsage {
    type Error = HidUsageParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            // Generic modifiers -- resolve to left-side defaults
            "Ctrl" => Ok(Self::LeftControl),
            "Shift" => Ok(Self::LeftShift),
            "Alt" | "Option" => Ok(Self::LeftAlt),
            "Command" | "Cmd" | "Super" => Ok(Self::LeftCommand),
            // Specific modifiers
            "LeftControl" | "LeftCtrl" => Ok(Self::LeftControl),
            "RightControl" | "RightCtrl" => Ok(Self::RightControl),
            "LeftShift" => Ok(Self::LeftShift),
            "RightShift" => Ok(Self::RightShift),
            "LeftAlt" | "LeftOption" => Ok(Self::LeftAlt),
            "RightAlt" | "RightOption" => Ok(Self::RightAlt),
            "LeftCommand" | "LeftCmd" => Ok(Self::LeftCommand),
            "RightCommand" | "RightCmd" => Ok(Self::RightCommand),
            // Non-modifier keys
            "CapsLock" | "Caps" => Ok(Self::CapsLock),
            "Tab" => Ok(Self::Tab),
            "Space" => Ok(Self::Space),
            "Return" | "Enter" => Ok(Self::Return),
            "Backspace" => Ok(Self::Backspace),
            "Delete" => Ok(Self::Delete),
            "Escape" | "Esc" => Ok(Self::Escape),
            "UpArrow" | "Up" => Ok(Self::UpArrow),
            "DownArrow" | "Down" => Ok(Self::DownArrow),
            "LeftArrow" | "Left" => Ok(Self::LeftArrow),
            "RightArrow" | "Right" => Ok(Self::RightArrow),
            "PageUp" | "PgUp" => Ok(Self::PageUp),
            "PageDown" | "PgDn" => Ok(Self::PageDown),
            "Home" => Ok(Self::Home),
            "End" => Ok(Self::End),
            "F1" => Ok(Self::F1),
            "F2" => Ok(Self::F2),
            "F3" => Ok(Self::F3),
            "F4" => Ok(Self::F4),
            "F5" => Ok(Self::F5),
            "F6" => Ok(Self::F6),
            "F7" => Ok(Self::F7),
            "F8" => Ok(Self::F8),
            "F9" => Ok(Self::F9),
            "F10" => Ok(Self::F10),
            "F11" => Ok(Self::F11),
            "F12" => Ok(Self::F12),
            "A" => Ok(Self::A),
            "B" => Ok(Self::B),
            "C" => Ok(Self::C),
            "D" => Ok(Self::D),
            "E" => Ok(Self::E),
            "F" => Ok(Self::F),
            "G" => Ok(Self::G),
            "H" => Ok(Self::H),
            "I" => Ok(Self::I),
            "J" => Ok(Self::J),
            "K" => Ok(Self::K),
            "L" => Ok(Self::L),
            "M" => Ok(Self::M),
            "N" => Ok(Self::N),
            "O" => Ok(Self::O),
            "P" => Ok(Self::P),
            "Q" => Ok(Self::Q),
            "R" => Ok(Self::R),
            "S" => Ok(Self::S),
            "T" => Ok(Self::T),
            "U" => Ok(Self::U),
            "V" => Ok(Self::V),
            "W" => Ok(Self::W),
            "X" => Ok(Self::X),
            "Y" => Ok(Self::Y),
            "Z" => Ok(Self::Z),
            "1" | "Number1" => Ok(Self::Number1),
            "2" | "Number2" => Ok(Self::Number2),
            "3" | "Number3" => Ok(Self::Number3),
            "4" | "Number4" => Ok(Self::Number4),
            "5" | "Number5" => Ok(Self::Number5),
            "6" | "Number6" => Ok(Self::Number6),
            "7" | "Number7" => Ok(Self::Number7),
            "8" | "Number8" => Ok(Self::Number8),
            "9" | "Number9" => Ok(Self::Number9),
            "0" | "Number0" => Ok(Self::Number0),
            // Numpad
            "Numpad0" => Ok(Self::Numpad0),
            "Numpad1" => Ok(Self::Numpad1),
            "Numpad2" => Ok(Self::Numpad2),
            "Numpad3" => Ok(Self::Numpad3),
            "Numpad4" => Ok(Self::Numpad4),
            "Numpad5" => Ok(Self::Numpad5),
            "Numpad6" => Ok(Self::Numpad6),
            "Numpad7" => Ok(Self::Numpad7),
            "Numpad8" => Ok(Self::Numpad8),
            "Numpad9" => Ok(Self::Numpad9),
            "NumpadDecimal" => Ok(Self::NumpadDecimal),
            "NumpadMultiply" | "KP_Multiply" => Ok(Self::NumpadMultiply),
            "NumpadPlus" | "KP_Add" => Ok(Self::NumpadPlus),
            "NumpadDivide" | "KP_Divide" => Ok(Self::NumpadDivide),
            "NumpadEnter" | "KP_Enter" => Ok(Self::NumpadEnter),
            "NumpadMinus" | "KP_Subtract" => Ok(Self::NumpadMinus),
            "NumpadClear" => Ok(Self::NumpadClear),
            "NumpadEqual" => Ok(Self::NumpadEqual),
            // Punctuation / symbols
            "Minus" => Ok(Self::Minus),
            "Equal" => Ok(Self::Equal),
            "BracketLeft" => Ok(Self::BracketLeft),
            "BracketRight" => Ok(Self::BracketRight),
            "Backslash" => Ok(Self::Backslash),
            "Semicolon" => Ok(Self::Semicolon),
            "Quote" => Ok(Self::Quote),
            "Comma" => Ok(Self::Comma),
            "Period" => Ok(Self::Period),
            "Slash" => Ok(Self::Slash),
            "Grave" => Ok(Self::Grave),
            "IsoExtra" | "NonUSBackslash" => Ok(Self::IsoExtra),
            "IsoHash" | "Hash" => Ok(Self::IsoHash),
            // Consumer page — media controls
            "PlayPause" | "Play" => Ok(Self::PlayPause),
            "VolumeUp" | "VolUp" => Ok(Self::VolumeUp),
            "VolumeDown" | "VolDown" => Ok(Self::VolumeDown),
            "Mute" | "VolMute" => Ok(Self::Mute),
            "NextTrack" | "ScanNext" => Ok(Self::NextTrack),
            "PreviousTrack" | "ScanPrev" => Ok(Self::PreviousTrack),
            "Stop" | "MediaStop" => Ok(Self::Stop),
            // Consumer page — display controls
            "BrightnessUp" => Ok(Self::BrightnessUp),
            "BrightnessDown" => Ok(Self::BrightnessDown),
            other => Err(HidUsageParseError(other.to_owned())),
        }
    }
}

impl From<HidUsage> for String {
    fn from(usage: HidUsage) -> Self {
        usage.as_str().to_owned()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to parse a string into `HidUsage`, panicking on failure.
    fn parse(name: &str) -> HidUsage {
        HidUsage::try_from(name)
            .unwrap_or_else(|e| panic!("failed to parse '{name}': {e}"))
    }

    #[test]
    fn try_from_canonical_names() {
        // Modifiers
        assert_eq!(parse("LeftControl"), HidUsage::LeftControl);
        assert_eq!(parse("RightControl"), HidUsage::RightControl);
        assert_eq!(parse("LeftShift"), HidUsage::LeftShift);
        assert_eq!(parse("RightShift"), HidUsage::RightShift);
        assert_eq!(parse("LeftAlt"), HidUsage::LeftAlt);
        assert_eq!(parse("RightAlt"), HidUsage::RightAlt);
        assert_eq!(parse("LeftCommand"), HidUsage::LeftCommand);
        assert_eq!(parse("RightCommand"), HidUsage::RightCommand);
        // Caps Lock
        assert_eq!(parse("CapsLock"), HidUsage::CapsLock);
        // Editor / misc
        assert_eq!(parse("Tab"), HidUsage::Tab);
        assert_eq!(parse("Space"), HidUsage::Space);
        assert_eq!(parse("Return"), HidUsage::Return);
        assert_eq!(parse("Backspace"), HidUsage::Backspace);
        assert_eq!(parse("Delete"), HidUsage::Delete);
        assert_eq!(parse("Escape"), HidUsage::Escape);
        // Navigation
        assert_eq!(parse("UpArrow"), HidUsage::UpArrow);
        assert_eq!(parse("DownArrow"), HidUsage::DownArrow);
        assert_eq!(parse("LeftArrow"), HidUsage::LeftArrow);
        assert_eq!(parse("RightArrow"), HidUsage::RightArrow);
        assert_eq!(parse("PageUp"), HidUsage::PageUp);
        assert_eq!(parse("PageDown"), HidUsage::PageDown);
        assert_eq!(parse("Home"), HidUsage::Home);
        assert_eq!(parse("End"), HidUsage::End);
        // Function keys
        assert_eq!(parse("F1"), HidUsage::F1);
        assert_eq!(parse("F12"), HidUsage::F12);
        // Letters
        assert_eq!(parse("A"), HidUsage::A);
        assert_eq!(parse("Z"), HidUsage::Z);
        // Numbers
        assert_eq!(parse("Number1"), HidUsage::Number1);
        assert_eq!(parse("Number0"), HidUsage::Number0);
        assert_eq!(parse("1"), HidUsage::Number1);
        assert_eq!(parse("0"), HidUsage::Number0);
        // Numpad
        assert_eq!(parse("Numpad0"), HidUsage::Numpad0);
        assert_eq!(parse("NumpadDecimal"), HidUsage::NumpadDecimal);
        assert_eq!(parse("NumpadMultiply"), HidUsage::NumpadMultiply);
        assert_eq!(parse("NumpadClear"), HidUsage::NumpadClear);
        assert_eq!(parse("NumpadEqual"), HidUsage::NumpadEqual);
        // Punctuation / symbols
        assert_eq!(parse("Minus"), HidUsage::Minus);
        assert_eq!(parse("Equal"), HidUsage::Equal);
        assert_eq!(parse("BracketLeft"), HidUsage::BracketLeft);
        assert_eq!(parse("BracketRight"), HidUsage::BracketRight);
        assert_eq!(parse("Backslash"), HidUsage::Backslash);
        assert_eq!(parse("Semicolon"), HidUsage::Semicolon);
        assert_eq!(parse("Quote"), HidUsage::Quote);
        assert_eq!(parse("Comma"), HidUsage::Comma);
        assert_eq!(parse("Period"), HidUsage::Period);
        assert_eq!(parse("Slash"), HidUsage::Slash);
        assert_eq!(parse("Grave"), HidUsage::Grave);
        assert_eq!(parse("IsoExtra"), HidUsage::IsoExtra);
        assert_eq!(parse("IsoHash"), HidUsage::IsoHash);
        // Consumer page
        assert_eq!(parse("PlayPause"), HidUsage::PlayPause);
        assert_eq!(parse("VolumeUp"), HidUsage::VolumeUp);
        assert_eq!(parse("VolumeDown"), HidUsage::VolumeDown);
        assert_eq!(parse("Mute"), HidUsage::Mute);
        assert_eq!(parse("NextTrack"), HidUsage::NextTrack);
        assert_eq!(parse("PreviousTrack"), HidUsage::PreviousTrack);
        assert_eq!(parse("Stop"), HidUsage::Stop);
        assert_eq!(parse("BrightnessUp"), HidUsage::BrightnessUp);
        assert_eq!(parse("BrightnessDown"), HidUsage::BrightnessDown);
    }

    #[test]
    fn try_from_aliases() {
        // Generic modifiers
        assert_eq!(parse("Ctrl"), HidUsage::LeftControl);
        assert_eq!(parse("Shift"), HidUsage::LeftShift);
        assert_eq!(parse("Alt"), HidUsage::LeftAlt);
        assert_eq!(parse("Option"), HidUsage::LeftAlt);
        assert_eq!(parse("Command"), HidUsage::LeftCommand);
        assert_eq!(parse("Cmd"), HidUsage::LeftCommand);
        assert_eq!(parse("Super"), HidUsage::LeftCommand);
        // Modifier aliases
        assert_eq!(parse("LeftCtrl"), HidUsage::LeftControl);
        assert_eq!(parse("RightCtrl"), HidUsage::RightControl);
        assert_eq!(parse("LeftOption"), HidUsage::LeftAlt);
        assert_eq!(parse("RightOption"), HidUsage::RightAlt);
        assert_eq!(parse("LeftCmd"), HidUsage::LeftCommand);
        assert_eq!(parse("RightCmd"), HidUsage::RightCommand);
        // Key aliases
        assert_eq!(parse("Caps"), HidUsage::CapsLock);
        assert_eq!(parse("Enter"), HidUsage::Return);
        assert_eq!(parse("Esc"), HidUsage::Escape);
        assert_eq!(parse("Up"), HidUsage::UpArrow);
        assert_eq!(parse("Down"), HidUsage::DownArrow);
        assert_eq!(parse("Left"), HidUsage::LeftArrow);
        assert_eq!(parse("Right"), HidUsage::RightArrow);
        assert_eq!(parse("PgUp"), HidUsage::PageUp);
        assert_eq!(parse("PgDn"), HidUsage::PageDown);
        // Numpad aliases
        assert_eq!(parse("KP_Multiply"), HidUsage::NumpadMultiply);
        assert_eq!(parse("KP_Add"), HidUsage::NumpadPlus);
        assert_eq!(parse("KP_Divide"), HidUsage::NumpadDivide);
        assert_eq!(parse("KP_Enter"), HidUsage::NumpadEnter);
        assert_eq!(parse("KP_Subtract"), HidUsage::NumpadMinus);
        // Punctuation aliases
        assert_eq!(parse("NonUSBackslash"), HidUsage::IsoExtra);
        assert_eq!(parse("Hash"), HidUsage::IsoHash);
        // Consumer page aliases
        assert_eq!(parse("Play"), HidUsage::PlayPause);
        assert_eq!(parse("VolUp"), HidUsage::VolumeUp);
        assert_eq!(parse("VolDown"), HidUsage::VolumeDown);
        assert_eq!(parse("VolMute"), HidUsage::Mute);
        assert_eq!(parse("ScanNext"), HidUsage::NextTrack);
        assert_eq!(parse("ScanPrev"), HidUsage::PreviousTrack);
        assert_eq!(parse("MediaStop"), HidUsage::Stop);
    }

    #[test]
    fn try_from_unknown_returns_err() {
        assert!(HidUsage::try_from("NonExistent").is_err());
        assert!(HidUsage::try_from("Xspace").is_err());
        // Error contains the input string
        let err = HidUsage::try_from("BadKey").unwrap_err();
        assert_eq!(err.0, "BadKey");
    }

    #[test]
    fn as_str_round_trip() {
        // Every variant round-trips through as_str / try_from.
        for usage in HidUsage::ALL.iter().copied() {
            let name = usage.as_str();
            assert_eq!(
                HidUsage::try_from(name),
                Ok(usage),
                "round-trip failed for {usage} -> \"{name}\"",
            );
        }
    }

    #[test]
    fn from_string_round_trip() {
        // Every variant converts to String and back.
        for usage in HidUsage::ALL.iter().copied() {
            let s: String = usage.into();
            assert_eq!(
                HidUsage::try_from(s.as_str()),
                Ok(usage),
                "String round-trip failed for {usage}",
            );
        }
    }

    #[test]
    fn consumer_page_serialization_round_trip() {
        // Every Consumer Page key serializes to its canonical name and
        // deserializes back to the same usage.  This guards the media and
        // display control keys that have no Keyboard Page equivalent.
        let consumer_keys: Vec<HidUsage> = HidUsage::ALL
            .iter()
            .copied()
            .filter(|u| u.page() == PAGE_CONSUMER)
            .collect();
        assert!(!consumer_keys.is_empty(), "expected consumer page keys");

        for usage in consumer_keys {
            let yaml = serde_yaml::to_string(&usage).unwrap_or_else(|e| {
                panic!("serialize {} failed: {e}", usage.as_str())
            });
            // The serialized form is the canonical name as a plain scalar.
            assert_eq!(
                yaml.trim(),
                usage.as_str(),
                "unexpected serialized form for {}",
                usage.as_str(),
            );
            let back: HidUsage =
                serde_yaml::from_str(&yaml).unwrap_or_else(|e| {
                    panic!("deserialize {yaml:?} failed: {e}")
                });
            assert_eq!(
                back,
                usage,
                "round-trip failed for {}",
                usage.as_str()
            );
        }
    }

    #[test]
    fn all_count() {
        assert_eq!(HidUsage::all().len(), 111);
    }

    #[test]
    fn code_and_page_id() {
        // Keyboard page key
        assert_eq!(HidUsage::A.code(), 0x070004);
        assert_eq!(HidUsage::A.page(), PAGE_KEYBOARD);
        assert_eq!(HidUsage::A.id(), 0x04);

        // Modifier
        assert_eq!(HidUsage::LeftControl.code(), 0x0700E0);
        assert_eq!(HidUsage::LeftControl.page(), PAGE_KEYBOARD);
        assert_eq!(HidUsage::LeftControl.id(), 0xE0);

        // Consumer page key
        assert_eq!(HidUsage::PlayPause.code(), 0x0C00CD);
        assert_eq!(HidUsage::PlayPause.page(), PAGE_CONSUMER);
        assert_eq!(HidUsage::PlayPause.id(), 0xCD);

        assert_eq!(HidUsage::VolumeUp.code(), 0x0C00E9);
        assert_eq!(HidUsage::VolumeUp.page(), PAGE_CONSUMER);
        assert_eq!(HidUsage::VolumeUp.id(), 0xE9);
    }

    #[test]
    fn from_code_round_trip() {
        // Every variant round-trips through code / from_code.
        for usage in HidUsage::ALL.iter().copied() {
            let code = usage.code();
            assert_eq!(
                HidUsage::from_code(code),
                Some(usage),
                "round-trip failed for {usage} -> 0x{code:08X}",
            );
        }

        // Unknown codes return None
        assert_eq!(HidUsage::from_code(0x070100), None);
        assert_eq!(HidUsage::from_code(0x0C0100), None);
        assert_eq!(HidUsage::from_code(0x000000), None);
    }

    #[test]
    fn keyboard_and_consumer_constructors() {
        // Valid keyboard id
        assert_eq!(HidUsage::keyboard(0x04), Some(HidUsage::A));
        assert_eq!(HidUsage::keyboard(0xE0), Some(HidUsage::LeftControl));

        // Valid consumer id
        assert_eq!(HidUsage::consumer(0xCD), Some(HidUsage::PlayPause));
        assert_eq!(HidUsage::consumer(0xE9), Some(HidUsage::VolumeUp));

        // Unknown ids return None
        assert_eq!(HidUsage::keyboard(0xFF), None);
        assert_eq!(HidUsage::consumer(0xFF), None);
    }

    #[test]
    fn hid_usage_to_modifier_bit_all_eight_modifiers() {
        // Every modifier usage maps to its `ModifierRole` bit position; the
        // canonical layout lives in `common::modifier`.
        let modifiers: [(HidUsage, u8); 8] = [
            (HidUsage::LeftControl, 0),
            (HidUsage::LeftShift, 1),
            (HidUsage::LeftAlt, 2),
            (HidUsage::LeftCommand, 3),
            (HidUsage::RightControl, 4),
            (HidUsage::RightShift, 5),
            (HidUsage::RightAlt, 6),
            (HidUsage::RightCommand, 7),
        ];
        for (usage, expected_bit) in modifiers {
            assert_eq!(
                HidUsage::hid_usage_to_modifier_bit(usage),
                Some(expected_bit),
                "wrong modifier bit for {}",
                usage.as_str(),
            );
        }
    }

    #[test]
    fn hid_usage_to_modifier_bit_non_modifier() {
        assert_eq!(
            HidUsage::hid_usage_to_modifier_bit(HidUsage::A),
            None, // A is not a modifier
        );
    }

    #[test]
    fn hid_usage_to_modifier_bit_consumer_page() {
        assert_eq!(
            HidUsage::hid_usage_to_modifier_bit(HidUsage::Mute),
            None, // consumer page, not a modifier
        );
    }

    #[test]
    fn display_known_usage() {
        assert_eq!(format!("{}", HidUsage::A), "A");
        assert_eq!(format!("{}", HidUsage::PlayPause), "PlayPause");
    }

    #[test]
    fn derive_traits() {
        let a = HidUsage::A;
        let b = HidUsage::A;
        let c = HidUsage::B;

        // PartialEq, Eq
        assert_eq!(a, b);
        assert_ne!(a, c);

        // PartialOrd, Ord
        assert!(a < c);

        // Hash (can be used in HashMap)
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert(a, "A");
        assert_eq!(map.get(&b), Some(&"A"));

        // Clone, Copy
        let _d: HidUsage = a;
    }

    #[test]
    fn no_duplicate_codes() {
        // Every variant has a unique discriminant value.
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for usage in HidUsage::ALL.iter().copied() {
            assert!(
                seen.insert(usage.code()),
                "duplicate code 0x{:08X} for {}",
                usage.code(),
                usage.as_str(),
            );
        }
    }

    #[test]
    fn no_duplicate_canonical_names() {
        // Every canonical name appears exactly once.
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for usage in HidUsage::ALL.iter().copied() {
            assert!(
                seen.insert(usage.as_str()),
                "duplicate canonical name '{}'",
                usage.as_str(),
            );
        }
    }

    #[test]
    fn hid_usage_values_match_spec() {
        // Spot-check HID usage values against the USB HID Usage Tables spec.
        assert_eq!(HidUsage::A.id(), 0x04);
        assert_eq!(HidUsage::Z.id(), 0x1D);
        assert_eq!(HidUsage::Number1.id(), 0x1E);
        assert_eq!(HidUsage::Return.id(), 0x28);
        assert_eq!(HidUsage::Escape.id(), 0x29);
        assert_eq!(HidUsage::Backspace.id(), 0x2A);
        assert_eq!(HidUsage::Tab.id(), 0x2B);
        assert_eq!(HidUsage::Space.id(), 0x2C);
        assert_eq!(HidUsage::LeftControl.id(), 0xE0);
        assert_eq!(HidUsage::LeftShift.id(), 0xE1);
        assert_eq!(HidUsage::LeftAlt.id(), 0xE2);
        assert_eq!(HidUsage::LeftCommand.id(), 0xE3);

        // Consumer page
        assert_eq!(HidUsage::PlayPause.id(), 0xCD);
        assert_eq!(HidUsage::VolumeUp.id(), 0xE9);
        assert_eq!(HidUsage::Mute.id(), 0xE2);
        assert_eq!(HidUsage::BrightnessUp.id(), 0x6F);
    }
}
