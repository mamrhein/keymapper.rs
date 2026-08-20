// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::common::{
    hid_usage::{HidUsage, PAGE_CONSUMER, PAGE_KEYBOARD},
    modifier::ModifierRole,
};

// ---------------------------------------------------------------------------
// Platform-specific Key enum — discriminants ARE the VK_* codes
// ---------------------------------------------------------------------------

/// Windows virtual-key code for a physical key on a US ANSI keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
pub enum Key {
    LeftControl = 0xA2,  // VK_LCONTROL
    RightControl = 0xA3, // VK_RCONTROL
    LeftShift = 0xA0,    // VK_LSHIFT
    RightShift = 0xA1,   // VK_RSHIFT
    LeftAlt = 0xA4,      // VK_LMENU
    RightAlt = 0xA5,     // VK_RMENU
    LeftCommand = 0x5B,  // VK_LWIN
    RightCommand = 0x5C, // VK_RWIN
    CapsLock = 0x14,     // VK_CAPITAL
    Tab = 0x09,          // VK_TAB
    Space = 0x20,        // VK_SPACE
    Return = 0x0D,       // VK_RETURN
    Backspace = 0x08,    // VK_BACK
    Delete = 0x2E,       // VK_DELETE
    Escape = 0x1B,       // VK_ESCAPE
    UpArrow = 0x26,      // VK_UP
    DownArrow = 0x28,    // VK_DOWN
    LeftArrow = 0x25,    // VK_LEFT
    RightArrow = 0x27,   // VK_RIGHT
    PageUp = 0x21,       // VK_PRIOR
    PageDown = 0x22,     // VK_NEXT
    Home = 0x23,         // VK_HOME
    End = 0x24,          // VK_END
    F1 = 0x70,
    F2 = 0x71,
    F3 = 0x72,
    F4 = 0x73,
    F5 = 0x74,
    F6 = 0x75,
    F7 = 0x76,
    F8 = 0x77,
    F9 = 0x78,
    F10 = 0x79,
    F11 = 0x7A,
    F12 = 0x7B,
    A = 0x41,
    B = 0x42,
    C = 0x43,
    D = 0x44,
    E = 0x45,
    F = 0x46,
    G = 0x47,
    H = 0x48,
    I = 0x49,
    J = 0x4A,
    K = 0x4B,
    L = 0x4C,
    M = 0x4D,
    N = 0x4E,
    O = 0x4F,
    P = 0x50,
    Q = 0x51,
    R = 0x52,
    S = 0x53,
    T = 0x54,
    U = 0x55,
    V = 0x56,
    W = 0x57,
    X = 0x58,
    Y = 0x59,
    Z = 0x5A,
    Number1 = 0x31,
    Number2 = 0x32,
    Number3 = 0x33,
    Number4 = 0x34,
    Number5 = 0x35,
    Number6 = 0x36,
    Number7 = 0x37,
    Number8 = 0x38,
    Number9 = 0x39,
    Number0 = 0x30,
    // --- Numpad ---
    Numpad0 = 0x60,        // VK_NUMPAD0
    Numpad1 = 0x61,        // VK_NUMPAD1
    Numpad2 = 0x62,        // VK_NUMPAD2
    Numpad3 = 0x63,        // VK_NUMPAD3
    Numpad4 = 0x64,        // VK_NUMPAD4
    Numpad5 = 0x65,        // VK_NUMPAD5
    Numpad6 = 0x66,        // VK_NUMPAD6
    Numpad7 = 0x67,        // VK_NUMPAD7
    Numpad8 = 0x68,        // VK_NUMPAD8
    Numpad9 = 0x69,        // VK_NUMPAD9
    NumpadDecimal = 0x6E,  // VK_DECIMAL
    NumpadMultiply = 0x6A, // VK_MULTIPLY
    NumpadPlus = 0x6B,     // VK_ADD
    NumpadDivide = 0x6F,   // VK_DIVIDE
    NumpadEnter = 0x92,    // VK_RETURN (extended)
    NumpadMinus = 0x6D,    // VK_SUBTRACT
    // --- Punctuation / symbols ---
    Minus = 0xBD,        // VK_OEM_MINUS
    Equal = 0xBB,        // VK_OEM_PLUS
    BracketLeft = 0xDB,  // VK_OEM_4
    BracketRight = 0xDD, // VK_OEM_6
    Backslash = 0xDC,    // VK_OEM_5
    Semicolon = 0xBA,    // VK_OEM_1
    Quote = 0xDE,        // VK_OEM_7
    Comma = 0xBC,        // VK_OEM_COMMA
    Period = 0xBE,       // VK_OEM_PERIOD
    Slash = 0xBF,        // VK_OEM_2
    Grave = 0xC0,        // VK_OEM_3
    IsoExtra = 0xE2,     // VK_OEM_102 (between Shift and Z on ISO)
    IsoHash = 0xDF,      // VK_OEM_8
    // --- Consumer page (media keys) ---
    PlayPause = 0xB3,     // VK_MEDIA_PLAY_PAUSE
    VolumeUp = 0xAF,      // VK_VOLUME_UP
    VolumeDown = 0xAE,    // VK_VOLUME_DOWN
    Mute = 0xAD,          // VK_VOLUME_MUTE
    NextTrack = 0xB0,     // VK_MEDIA_NEXT_TRACK
    PreviousTrack = 0xB1, // VK_MEDIA_PREV_TRACK
    Stop = 0xB2,          // VK_MEDIA_STOP
}

impl Key {
    pub const fn as_native(self) -> u16 {
        self as u16
    }

    pub const fn as_modifier_bit(self) -> Option<u8> {
        let role = match self {
            Self::LeftControl => ModifierRole::LeftControl,
            Self::RightControl => ModifierRole::RightControl,
            Self::LeftShift => ModifierRole::LeftShift,
            Self::RightShift => ModifierRole::RightShift,
            Self::LeftAlt => ModifierRole::LeftAlt,
            Self::RightAlt => ModifierRole::RightAlt,
            Self::LeftCommand => ModifierRole::LeftCommand,
            Self::RightCommand => ModifierRole::RightCommand,
            _ => return None,
        };
        Some(role.bit())
    }

    pub fn as_modifier_positions(self) -> Option<Vec<u8>> {
        let role = match self {
            Self::LeftControl => ModifierRole::LeftControl,
            Self::RightControl => ModifierRole::RightControl,
            Self::LeftShift => ModifierRole::LeftShift,
            Self::RightShift => ModifierRole::RightShift,
            Self::LeftAlt => ModifierRole::LeftAlt,
            Self::RightAlt => ModifierRole::RightAlt,
            Self::LeftCommand => ModifierRole::LeftCommand,
            Self::RightCommand => ModifierRole::RightCommand,
            _ => return None,
        };
        let (a, b) = role.family_positions();
        Some(vec![a, b])
    }

    /// Convert a `HidUsage` to the Windows-native variant.
    ///
    /// Only Keyboard/Keypad page usages with a native `Key` variant are
    /// resolvable; Consumer Page usages (media keys) return `None` — use
    /// [`hid_to_vk`] for those — as do usages without a `Key` variant
    /// (`NumpadClear`, `NumpadEqual`).
    pub fn from_hid_usage(usage: HidUsage) -> Option<Self> {
        if usage.page() != PAGE_KEYBOARD {
            return None;
        }
        Some(match usage {
            HidUsage::LeftControl => Self::LeftControl,
            HidUsage::RightControl => Self::RightControl,
            HidUsage::LeftShift => Self::LeftShift,
            HidUsage::RightShift => Self::RightShift,
            HidUsage::LeftAlt => Self::LeftAlt,
            HidUsage::RightAlt => Self::RightAlt,
            HidUsage::LeftCommand => Self::LeftCommand,
            HidUsage::RightCommand => Self::RightCommand,
            HidUsage::CapsLock => Self::CapsLock,
            HidUsage::Tab => Self::Tab,
            HidUsage::Space => Self::Space,
            HidUsage::Return => Self::Return,
            HidUsage::Backspace => Self::Backspace,
            HidUsage::Delete => Self::Delete,
            HidUsage::Escape => Self::Escape,
            HidUsage::UpArrow => Self::UpArrow,
            HidUsage::DownArrow => Self::DownArrow,
            HidUsage::LeftArrow => Self::LeftArrow,
            HidUsage::RightArrow => Self::RightArrow,
            HidUsage::PageUp => Self::PageUp,
            HidUsage::PageDown => Self::PageDown,
            HidUsage::Home => Self::Home,
            HidUsage::End => Self::End,
            HidUsage::F1 => Self::F1,
            HidUsage::F2 => Self::F2,
            HidUsage::F3 => Self::F3,
            HidUsage::F4 => Self::F4,
            HidUsage::F5 => Self::F5,
            HidUsage::F6 => Self::F6,
            HidUsage::F7 => Self::F7,
            HidUsage::F8 => Self::F8,
            HidUsage::F9 => Self::F9,
            HidUsage::F10 => Self::F10,
            HidUsage::F11 => Self::F11,
            HidUsage::F12 => Self::F12,
            HidUsage::A => Self::A,
            HidUsage::B => Self::B,
            HidUsage::C => Self::C,
            HidUsage::D => Self::D,
            HidUsage::E => Self::E,
            HidUsage::F => Self::F,
            HidUsage::G => Self::G,
            HidUsage::H => Self::H,
            HidUsage::I => Self::I,
            HidUsage::J => Self::J,
            HidUsage::K => Self::K,
            HidUsage::L => Self::L,
            HidUsage::M => Self::M,
            HidUsage::N => Self::N,
            HidUsage::O => Self::O,
            HidUsage::P => Self::P,
            HidUsage::Q => Self::Q,
            HidUsage::R => Self::R,
            HidUsage::S => Self::S,
            HidUsage::T => Self::T,
            HidUsage::U => Self::U,
            HidUsage::V => Self::V,
            HidUsage::W => Self::W,
            HidUsage::X => Self::X,
            HidUsage::Y => Self::Y,
            HidUsage::Z => Self::Z,
            HidUsage::Number1 => Self::Number1,
            HidUsage::Number2 => Self::Number2,
            HidUsage::Number3 => Self::Number3,
            HidUsage::Number4 => Self::Number4,
            HidUsage::Number5 => Self::Number5,
            HidUsage::Number6 => Self::Number6,
            HidUsage::Number7 => Self::Number7,
            HidUsage::Number8 => Self::Number8,
            HidUsage::Number9 => Self::Number9,
            HidUsage::Number0 => Self::Number0,
            HidUsage::Numpad0 => Self::Numpad0,
            HidUsage::Numpad1 => Self::Numpad1,
            HidUsage::Numpad2 => Self::Numpad2,
            HidUsage::Numpad3 => Self::Numpad3,
            HidUsage::Numpad4 => Self::Numpad4,
            HidUsage::Numpad5 => Self::Numpad5,
            HidUsage::Numpad6 => Self::Numpad6,
            HidUsage::Numpad7 => Self::Numpad7,
            HidUsage::Numpad8 => Self::Numpad8,
            HidUsage::Numpad9 => Self::Numpad9,
            HidUsage::NumpadDecimal => Self::NumpadDecimal,
            HidUsage::NumpadMultiply => Self::NumpadMultiply,
            HidUsage::NumpadPlus => Self::NumpadPlus,
            HidUsage::NumpadDivide => Self::NumpadDivide,
            HidUsage::NumpadEnter => Self::NumpadEnter,
            HidUsage::NumpadMinus => Self::NumpadMinus,
            HidUsage::Minus => Self::Minus,
            HidUsage::Equal => Self::Equal,
            HidUsage::BracketLeft => Self::BracketLeft,
            HidUsage::BracketRight => Self::BracketRight,
            HidUsage::Backslash => Self::Backslash,
            HidUsage::Semicolon => Self::Semicolon,
            HidUsage::Quote => Self::Quote,
            HidUsage::Comma => Self::Comma,
            HidUsage::Period => Self::Period,
            HidUsage::Slash => Self::Slash,
            HidUsage::Grave => Self::Grave,
            HidUsage::IsoExtra => Self::IsoExtra,
            HidUsage::IsoHash => Self::IsoHash,
            // NumpadClear, NumpadEqual, and all consumer page usages have
            // no native variant.
            _ => return None,
        })
    }

    /// Convert the Windows-native variant to its `HidUsage`.
    ///
    /// Every `Key` variant maps to a HID usage — keyboard-page variants to
    /// their Keyboard/Keypad usage and media variants to their Consumer
    /// Page usage — so this never fails.
    pub fn to_hid_usage(self) -> HidUsage {
        match self {
            Self::LeftControl => HidUsage::LeftControl,
            Self::RightControl => HidUsage::RightControl,
            Self::LeftShift => HidUsage::LeftShift,
            Self::RightShift => HidUsage::RightShift,
            Self::LeftAlt => HidUsage::LeftAlt,
            Self::RightAlt => HidUsage::RightAlt,
            Self::LeftCommand => HidUsage::LeftCommand,
            Self::RightCommand => HidUsage::RightCommand,
            Self::CapsLock => HidUsage::CapsLock,
            Self::Tab => HidUsage::Tab,
            Self::Space => HidUsage::Space,
            Self::Return => HidUsage::Return,
            Self::Backspace => HidUsage::Backspace,
            Self::Delete => HidUsage::Delete,
            Self::Escape => HidUsage::Escape,
            Self::UpArrow => HidUsage::UpArrow,
            Self::DownArrow => HidUsage::DownArrow,
            Self::LeftArrow => HidUsage::LeftArrow,
            Self::RightArrow => HidUsage::RightArrow,
            Self::PageUp => HidUsage::PageUp,
            Self::PageDown => HidUsage::PageDown,
            Self::Home => HidUsage::Home,
            Self::End => HidUsage::End,
            Self::F1 => HidUsage::F1,
            Self::F2 => HidUsage::F2,
            Self::F3 => HidUsage::F3,
            Self::F4 => HidUsage::F4,
            Self::F5 => HidUsage::F5,
            Self::F6 => HidUsage::F6,
            Self::F7 => HidUsage::F7,
            Self::F8 => HidUsage::F8,
            Self::F9 => HidUsage::F9,
            Self::F10 => HidUsage::F10,
            Self::F11 => HidUsage::F11,
            Self::F12 => HidUsage::F12,
            Self::A => HidUsage::A,
            Self::B => HidUsage::B,
            Self::C => HidUsage::C,
            Self::D => HidUsage::D,
            Self::E => HidUsage::E,
            Self::F => HidUsage::F,
            Self::G => HidUsage::G,
            Self::H => HidUsage::H,
            Self::I => HidUsage::I,
            Self::J => HidUsage::J,
            Self::K => HidUsage::K,
            Self::L => HidUsage::L,
            Self::M => HidUsage::M,
            Self::N => HidUsage::N,
            Self::O => HidUsage::O,
            Self::P => HidUsage::P,
            Self::Q => HidUsage::Q,
            Self::R => HidUsage::R,
            Self::S => HidUsage::S,
            Self::T => HidUsage::T,
            Self::U => HidUsage::U,
            Self::V => HidUsage::V,
            Self::W => HidUsage::W,
            Self::X => HidUsage::X,
            Self::Y => HidUsage::Y,
            Self::Z => HidUsage::Z,
            Self::Number1 => HidUsage::Number1,
            Self::Number2 => HidUsage::Number2,
            Self::Number3 => HidUsage::Number3,
            Self::Number4 => HidUsage::Number4,
            Self::Number5 => HidUsage::Number5,
            Self::Number6 => HidUsage::Number6,
            Self::Number7 => HidUsage::Number7,
            Self::Number8 => HidUsage::Number8,
            Self::Number9 => HidUsage::Number9,
            Self::Number0 => HidUsage::Number0,
            Self::Numpad0 => HidUsage::Numpad0,
            Self::Numpad1 => HidUsage::Numpad1,
            Self::Numpad2 => HidUsage::Numpad2,
            Self::Numpad3 => HidUsage::Numpad3,
            Self::Numpad4 => HidUsage::Numpad4,
            Self::Numpad5 => HidUsage::Numpad5,
            Self::Numpad6 => HidUsage::Numpad6,
            Self::Numpad7 => HidUsage::Numpad7,
            Self::Numpad8 => HidUsage::Numpad8,
            Self::Numpad9 => HidUsage::Numpad9,
            Self::NumpadDecimal => HidUsage::NumpadDecimal,
            Self::NumpadMultiply => HidUsage::NumpadMultiply,
            Self::NumpadPlus => HidUsage::NumpadPlus,
            Self::NumpadDivide => HidUsage::NumpadDivide,
            Self::NumpadEnter => HidUsage::NumpadEnter,
            Self::NumpadMinus => HidUsage::NumpadMinus,
            Self::Minus => HidUsage::Minus,
            Self::Equal => HidUsage::Equal,
            Self::BracketLeft => HidUsage::BracketLeft,
            Self::BracketRight => HidUsage::BracketRight,
            Self::Backslash => HidUsage::Backslash,
            Self::Semicolon => HidUsage::Semicolon,
            Self::Quote => HidUsage::Quote,
            Self::Comma => HidUsage::Comma,
            Self::Period => HidUsage::Period,
            Self::Slash => HidUsage::Slash,
            Self::Grave => HidUsage::Grave,
            Self::IsoExtra => HidUsage::IsoExtra,
            Self::IsoHash => HidUsage::IsoHash,
            Self::PlayPause => HidUsage::PlayPause,
            Self::VolumeUp => HidUsage::VolumeUp,
            Self::VolumeDown => HidUsage::VolumeDown,
            Self::Mute => HidUsage::Mute,
            Self::NextTrack => HidUsage::NextTrack,
            Self::PreviousTrack => HidUsage::PreviousTrack,
            Self::Stop => HidUsage::Stop,
        }
    }
}

/// Map a Consumer Page HID usage to the Windows virtual-key code used for
/// emission via `SendInput`.
///
/// Keyboard page usages have no entry here; use [`Key::from_hid_usage`]
/// for those.  Brightness keys have no standard virtual key, so they
/// return `None`.
pub fn hid_to_vk(usage: HidUsage) -> Option<u16> {
    match (usage.page(), usage.id()) {
        (PAGE_CONSUMER, 0xCD) => Some(0xB3), // VK_MEDIA_PLAY_PAUSE
        (PAGE_CONSUMER, 0xE9) => Some(0xAF), // VK_VOLUME_UP
        (PAGE_CONSUMER, 0xEA) => Some(0xAE), // VK_VOLUME_DOWN
        (PAGE_CONSUMER, 0xE2) => Some(0xAD), // VK_VOLUME_MUTE
        (PAGE_CONSUMER, 0xB5) => Some(0xB0), // VK_MEDIA_NEXT_TRACK
        (PAGE_CONSUMER, 0xB6) => Some(0xB1), // VK_MEDIA_PREV_TRACK
        (PAGE_CONSUMER, 0xB7) => Some(0xB2), // VK_MEDIA_STOP
        _ => None,
    }
}

impl Key {
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
            Self::PlayPause => "PlayPause",
            Self::VolumeUp => "VolumeUp",
            Self::VolumeDown => "VolumeDown",
            Self::Mute => "Mute",
            Self::NextTrack => "NextTrack",
            Self::PreviousTrack => "PreviousTrack",
            Self::Stop => "Stop",
        }
    }

    /// All defined key variants.
    pub const ALL: [Self; 107] = [
        // Modifiers
        Self::LeftControl,
        Self::RightControl,
        Self::LeftShift,
        Self::RightShift,
        Self::LeftAlt,
        Self::RightAlt,
        Self::LeftCommand,
        Self::RightCommand,
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
        // Punctuation / symbols
        Self::Minus,
        Self::Equal,
        Self::BracketLeft,
        Self::BracketRight,
        Self::Backslash,
        Self::Semicolon,
        Self::Quote,
        Self::Comma,
        Self::Period,
        Self::Slash,
        Self::Grave,
        Self::IsoExtra,
        Self::IsoHash,
        // Consumer page (media keys)
        Self::PlayPause,
        Self::VolumeUp,
        Self::VolumeDown,
        Self::Mute,
        Self::NextTrack,
        Self::PreviousTrack,
        Self::Stop,
    ];

    /// Convert a native virtual-key code back to a Key variant.
    ///
    /// Returns `None` for codes that are not defined in this enum.
    pub const fn from_native(code: u16) -> Option<Self> {
        match code {
            0xA2 => Some(Self::LeftControl),
            0xA3 => Some(Self::RightControl),
            0xA0 => Some(Self::LeftShift),
            0xA1 => Some(Self::RightShift),
            0xA4 => Some(Self::LeftAlt),
            0xA5 => Some(Self::RightAlt),
            0x5B => Some(Self::LeftCommand),
            0x5C => Some(Self::RightCommand),
            0x14 => Some(Self::CapsLock),
            0x09 => Some(Self::Tab),
            0x20 => Some(Self::Space),
            0x0D => Some(Self::Return),
            0x08 => Some(Self::Backspace),
            0x2E => Some(Self::Delete),
            0x1B => Some(Self::Escape),
            0x26 => Some(Self::UpArrow),
            0x28 => Some(Self::DownArrow),
            0x25 => Some(Self::LeftArrow),
            0x27 => Some(Self::RightArrow),
            0x21 => Some(Self::PageUp),
            0x22 => Some(Self::PageDown),
            0x23 => Some(Self::Home),
            0x24 => Some(Self::End),
            0x70 => Some(Self::F1),
            0x71 => Some(Self::F2),
            0x72 => Some(Self::F3),
            0x73 => Some(Self::F4),
            0x74 => Some(Self::F5),
            0x75 => Some(Self::F6),
            0x76 => Some(Self::F7),
            0x77 => Some(Self::F8),
            0x78 => Some(Self::F9),
            0x79 => Some(Self::F10),
            0x7A => Some(Self::F11),
            0x7B => Some(Self::F12),
            0x41 => Some(Self::A),
            0x42 => Some(Self::B),
            0x43 => Some(Self::C),
            0x44 => Some(Self::D),
            0x45 => Some(Self::E),
            0x46 => Some(Self::F),
            0x47 => Some(Self::G),
            0x48 => Some(Self::H),
            0x49 => Some(Self::I),
            0x4A => Some(Self::J),
            0x4B => Some(Self::K),
            0x4C => Some(Self::L),
            0x4D => Some(Self::M),
            0x4E => Some(Self::N),
            0x4F => Some(Self::O),
            0x50 => Some(Self::P),
            0x51 => Some(Self::Q),
            0x52 => Some(Self::R),
            0x53 => Some(Self::S),
            0x54 => Some(Self::T),
            0x55 => Some(Self::U),
            0x56 => Some(Self::V),
            0x57 => Some(Self::W),
            0x58 => Some(Self::X),
            0x59 => Some(Self::Y),
            0x5A => Some(Self::Z),
            0x31 => Some(Self::Number1),
            0x32 => Some(Self::Number2),
            0x33 => Some(Self::Number3),
            0x34 => Some(Self::Number4),
            0x35 => Some(Self::Number5),
            0x36 => Some(Self::Number6),
            0x37 => Some(Self::Number7),
            0x38 => Some(Self::Number8),
            0x39 => Some(Self::Number9),
            0x30 => Some(Self::Number0),
            0x60 => Some(Self::Numpad0),
            0x61 => Some(Self::Numpad1),
            0x62 => Some(Self::Numpad2),
            0x63 => Some(Self::Numpad3),
            0x64 => Some(Self::Numpad4),
            0x65 => Some(Self::Numpad5),
            0x66 => Some(Self::Numpad6),
            0x67 => Some(Self::Numpad7),
            0x68 => Some(Self::Numpad8),
            0x69 => Some(Self::Numpad9),
            0x6E => Some(Self::NumpadDecimal),
            0x6A => Some(Self::NumpadMultiply),
            0x6B => Some(Self::NumpadPlus),
            0x6F => Some(Self::NumpadDivide),
            0x92 => Some(Self::NumpadEnter),
            0x6D => Some(Self::NumpadMinus),
            0xBD => Some(Self::Minus),
            0xBB => Some(Self::Equal),
            0xDB => Some(Self::BracketLeft),
            0xDD => Some(Self::BracketRight),
            0xDC => Some(Self::Backslash),
            0xBA => Some(Self::Semicolon),
            0xDE => Some(Self::Quote),
            0xBC => Some(Self::Comma),
            0xBE => Some(Self::Period),
            0xBF => Some(Self::Slash),
            0xC0 => Some(Self::Grave),
            0xE2 => Some(Self::IsoExtra),
            0xDF => Some(Self::IsoHash),
            0xB3 => Some(Self::PlayPause),
            0xAF => Some(Self::VolumeUp),
            0xAE => Some(Self::VolumeDown),
            0xAD => Some(Self::Mute),
            0xB0 => Some(Self::NextTrack),
            0xB1 => Some(Self::PreviousTrack),
            0xB2 => Some(Self::Stop),
            _ => None,
        }
    }

    pub fn try_from_str(name: &str) -> Option<Self> {
        match name {
            "Ctrl" => Some(Self::LeftControl),
            "Shift" => Some(Self::LeftShift),
            "Alt" | "Option" => Some(Self::LeftAlt),
            "Command" | "Cmd" | "Super" | "Win" => Some(Self::LeftCommand),
            "LeftControl" | "LeftCtrl" => Some(Self::LeftControl),
            "RightControl" | "RightCtrl" => Some(Self::RightControl),
            "LeftShift" => Some(Self::LeftShift),
            "RightShift" => Some(Self::RightShift),
            "LeftAlt" | "LeftOption" => Some(Self::LeftAlt),
            "RightAlt" | "RightOption" => Some(Self::RightAlt),
            "LeftCommand" | "LeftCmd" | "LeftWin" => Some(Self::LeftCommand),
            "RightCommand" | "RightCmd" | "RightWin" => {
                Some(Self::RightCommand)
            }
            "CapsLock" | "Caps" => Some(Self::CapsLock),
            "Tab" => Some(Self::Tab),
            "Space" => Some(Self::Space),
            "Return" | "Enter" => Some(Self::Return),
            "Backspace" => Some(Self::Backspace),
            "Delete" => Some(Self::Delete),
            "Escape" | "Esc" => Some(Self::Escape),
            "UpArrow" | "Up" => Some(Self::UpArrow),
            "DownArrow" | "Down" => Some(Self::DownArrow),
            "LeftArrow" | "Left" => Some(Self::LeftArrow),
            "RightArrow" | "Right" => Some(Self::RightArrow),
            "PageUp" | "PgUp" => Some(Self::PageUp),
            "PageDown" | "PgDn" => Some(Self::PageDown),
            "Home" => Some(Self::Home),
            "End" => Some(Self::End),
            "F1" => Some(Self::F1),
            "F2" => Some(Self::F2),
            "F3" => Some(Self::F3),
            "F4" => Some(Self::F4),
            "F5" => Some(Self::F5),
            "F6" => Some(Self::F6),
            "F7" => Some(Self::F7),
            "F8" => Some(Self::F8),
            "F9" => Some(Self::F9),
            "F10" => Some(Self::F10),
            "F11" => Some(Self::F11),
            "F12" => Some(Self::F12),
            "A" => Some(Self::A),
            "B" => Some(Self::B),
            "C" => Some(Self::C),
            "D" => Some(Self::D),
            "E" => Some(Self::E),
            "F" => Some(Self::F),
            "G" => Some(Self::G),
            "H" => Some(Self::H),
            "I" => Some(Self::I),
            "J" => Some(Self::J),
            "K" => Some(Self::K),
            "L" => Some(Self::L),
            "M" => Some(Self::M),
            "N" => Some(Self::N),
            "O" => Some(Self::O),
            "P" => Some(Self::P),
            "Q" => Some(Self::Q),
            "R" => Some(Self::R),
            "S" => Some(Self::S),
            "T" => Some(Self::T),
            "U" => Some(Self::U),
            "V" => Some(Self::V),
            "W" => Some(Self::W),
            "X" => Some(Self::X),
            "Y" => Some(Self::Y),
            "Z" => Some(Self::Z),
            "1" | "Number1" => Some(Self::Number1),
            "2" | "Number2" => Some(Self::Number2),
            "3" | "Number3" => Some(Self::Number3),
            "4" | "Number4" => Some(Self::Number4),
            "5" | "Number5" => Some(Self::Number5),
            "6" | "Number6" => Some(Self::Number6),
            "7" | "Number7" => Some(Self::Number7),
            "8" | "Number8" => Some(Self::Number8),
            "9" | "Number9" => Some(Self::Number9),
            "0" | "Number0" => Some(Self::Number0),
            // Numpad
            "Numpad0" => Some(Self::Numpad0),
            "Numpad1" => Some(Self::Numpad1),
            "Numpad2" => Some(Self::Numpad2),
            "Numpad3" => Some(Self::Numpad3),
            "Numpad4" => Some(Self::Numpad4),
            "Numpad5" => Some(Self::Numpad5),
            "Numpad6" => Some(Self::Numpad6),
            "Numpad7" => Some(Self::Numpad7),
            "Numpad8" => Some(Self::Numpad8),
            "Numpad9" => Some(Self::Numpad9),
            "NumpadDecimal" => Some(Self::NumpadDecimal),
            "NumpadMultiply" => Some(Self::NumpadMultiply),
            "NumpadPlus" => Some(Self::NumpadPlus),
            "NumpadDivide" => Some(Self::NumpadDivide),
            "NumpadEnter" => Some(Self::NumpadEnter),
            "NumpadMinus" => Some(Self::NumpadMinus),
            // Punctuation / symbols
            "Minus" => Some(Self::Minus),
            "Equal" => Some(Self::Equal),
            "BracketLeft" => Some(Self::BracketLeft),
            "BracketRight" => Some(Self::BracketRight),
            "Backslash" => Some(Self::Backslash),
            "Semicolon" => Some(Self::Semicolon),
            "Quote" => Some(Self::Quote),
            "Comma" => Some(Self::Comma),
            "Period" => Some(Self::Period),
            "Slash" => Some(Self::Slash),
            "Grave" => Some(Self::Grave),
            "IsoExtra" => Some(Self::IsoExtra),
            "IsoHash" => Some(Self::IsoHash),
            // Consumer page (media keys)
            "PlayPause" | "Play" => Some(Self::PlayPause),
            "VolumeUp" | "VolUp" => Some(Self::VolumeUp),
            "VolumeDown" | "VolDown" => Some(Self::VolumeDown),
            "Mute" | "VolMute" => Some(Self::Mute),
            "NextTrack" | "ScanNext" => Some(Self::NextTrack),
            "PreviousTrack" | "ScanPrev" => Some(Self::PreviousTrack),
            "Stop" | "MediaStop" => Some(Self::Stop),
            _ => None,
        }
    }
}

impl Serialize for Key {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::try_from_str(&s).ok_or_else(|| {
            serde::de::Error::custom(crate::common::key::unknown_key_error(&s))
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_hid_usage_round_trips_for_all_variants() {
        // Every Key variant converts to a HidUsage whose reverse lookup
        // yields the original variant, except consumer variants which
        // resolve through hid_to_vk instead of from_hid_usage.
        for key in Key::ALL {
            let usage = key.to_hid_usage();
            if usage.page() == PAGE_KEYBOARD {
                assert_eq!(
                    Key::from_hid_usage(usage),
                    Some(key),
                    "round-trip failed for {:?}",
                    key
                );
            } else {
                assert_eq!(Key::from_hid_usage(usage), None);
                assert!(
                    hid_to_vk(usage).is_some(),
                    "no VK for consumer variant {:?}",
                    key
                );
            }
        }
    }

    #[test]
    fn from_hid_usage_returns_none_for_consumer_page() {
        assert_eq!(Key::from_hid_usage(HidUsage::PlayPause), None);
        assert_eq!(Key::from_hid_usage(HidUsage::VolumeUp), None);
        assert_eq!(Key::from_hid_usage(HidUsage::Mute), None);
    }

    #[test]
    fn from_hid_usage_returns_none_for_unsupported_keyboard_keys() {
        // NumpadClear and NumpadEqual have no VK_* equivalent.
        assert_eq!(Key::from_hid_usage(HidUsage::NumpadClear), None);
        assert_eq!(Key::from_hid_usage(HidUsage::NumpadEqual), None);
    }

    #[test]
    fn hid_to_vk_maps_consumer_usages() {
        assert_eq!(hid_to_vk(HidUsage::PlayPause), Some(0xB3));
        assert_eq!(hid_to_vk(HidUsage::VolumeUp), Some(0xAF));
        assert_eq!(hid_to_vk(HidUsage::VolumeDown), Some(0xAE));
        assert_eq!(hid_to_vk(HidUsage::Mute), Some(0xAD));
        assert_eq!(hid_to_vk(HidUsage::NextTrack), Some(0xB0));
        assert_eq!(hid_to_vk(HidUsage::PreviousTrack), Some(0xB1));
        assert_eq!(hid_to_vk(HidUsage::Stop), Some(0xB2));
    }

    #[test]
    fn hid_to_vk_returns_none_for_keyboard_page() {
        assert_eq!(hid_to_vk(HidUsage::A), None);
        assert_eq!(hid_to_vk(HidUsage::CapsLock), None);
    }

    #[test]
    fn hid_to_vk_returns_none_for_brightness_keys() {
        // Brightness keys have no standard virtual key.
        assert_eq!(hid_to_vk(HidUsage::BrightnessUp), None);
        assert_eq!(hid_to_vk(HidUsage::BrightnessDown), None);
    }

    #[test]
    fn from_native_recognises_media_keys() {
        assert_eq!(Key::from_native(0xB3), Some(Key::PlayPause));
        assert_eq!(Key::from_native(0xAF), Some(Key::VolumeUp));
        assert_eq!(Key::from_native(0xAE), Some(Key::VolumeDown));
        assert_eq!(Key::from_native(0xAD), Some(Key::Mute));
        assert_eq!(Key::from_native(0xB0), Some(Key::NextTrack));
        assert_eq!(Key::from_native(0xB1), Some(Key::PreviousTrack));
        assert_eq!(Key::from_native(0xB2), Some(Key::Stop));
    }
}
