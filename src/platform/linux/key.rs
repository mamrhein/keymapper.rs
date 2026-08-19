// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::common::{hid_usage::HidUsage, modifier::ModifierRole};

// ---------------------------------------------------------------------------
// Platform-specific Key enum — discriminants ARE the evdev KEY_* codes
// ---------------------------------------------------------------------------

/// Linux evdev keycode for a physical key on a US ANSI keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
pub enum Key {
    LeftControl = 29,   // KEY_LEFTCTRL
    RightControl = 97,  // KEY_RIGHTCTRL
    LeftShift = 42,     // KEY_LEFTSHIFT
    RightShift = 54,    // KEY_RIGHTSHIFT
    LeftAlt = 56,       // KEY_LEFTALT
    RightAlt = 100,     // KEY_RIGHTALT
    LeftCommand = 125,  // KEY_LEFTMETA
    RightCommand = 126, // KEY_RIGHTMETA
    CapsLock = 58,      // KEY_CAPSLOCK
    Tab = 15,           // KEY_TAB
    Space = 57,         // KEY_SPACE
    Return = 28,        // KEY_ENTER
    Backspace = 14,     // KEY_BACKSPACE
    Delete = 111,       // KEY_DELETE
    Escape = 1,         // KEY_ESC
    UpArrow = 103,      // KEY_UP
    DownArrow = 108,    // KEY_DOWN
    LeftArrow = 105,    // KEY_LEFT
    RightArrow = 106,   // KEY_RIGHT
    PageUp = 104,       // KEY_PAGEUP
    PageDown = 109,     // KEY_PAGEDOWN
    Home = 102,         // KEY_HOME
    End = 107,          // KEY_END
    F1 = 59,            // KEY_F1
    F2 = 60,            // KEY_F2
    F3 = 61,            // KEY_F3
    F4 = 62,            // KEY_F4
    F5 = 63,            // KEY_F5
    F6 = 64,            // KEY_F6
    F7 = 65,            // KEY_F7
    F8 = 66,            // KEY_F8
    F9 = 67,            // KEY_F9
    F10 = 68,           // KEY_F10
    F11 = 87,           // KEY_F11
    F12 = 88,           // KEY_F12
    A = 30,
    B = 48,
    C = 46,
    D = 32,
    E = 18,
    F = 33,
    G = 34,
    H = 35,
    I = 23,
    J = 36,
    K = 37,
    L = 38,
    M = 50,
    N = 49,
    O = 24,
    P = 25,
    Q = 16,
    R = 19,
    S = 31,
    T = 20,
    U = 22,
    V = 47,
    W = 17,
    X = 45,
    Y = 21,
    Z = 44,
    Number1 = 2,
    Number2 = 3,
    Number3 = 4,
    Number4 = 5,
    Number5 = 6,
    Number6 = 7,
    Number7 = 8,
    Number8 = 9,
    Number9 = 10,
    Number0 = 11,
    // --- Numpad ---
    Numpad7 = 71,       // KEY_KP7
    Numpad8 = 72,       // KEY_KP8
    Numpad9 = 73,       // KEY_KP9
    Numpad4 = 75,       // KEY_KP4
    Numpad5 = 76,       // KEY_KP5
    Numpad6 = 77,       // KEY_KP6
    Numpad1 = 79,       // KEY_KP1
    Numpad2 = 80,       // KEY_KP2
    Numpad3 = 81,       // KEY_KP3
    Numpad0 = 82,       // KEY_KP0
    NumpadDecimal = 83, // KEY_KPDOT
    NumpadPlus = 78,    // KEY_KPPLUS
    NumpadEnter = 96,   // KEY_KPENTER
    // Note: NumpadMultiply (KEY_KPASTERISK=55) shares code with F(55)
    // Note: NumpadMinus (KEY_KPMINUS=74) shares code with... nothing in our
    // enum
    NumpadMinus = 74,    // KEY_KPMINUS
    NumpadMultiply = 55, // KEY_KPASTERISK (shares evdev code with F)
    NumpadDivide = 98,   // KEY_KPSLASH
    // --- Punctuation / symbols ---
    Minus = 12,        // KEY_MINUS
    Equal = 13,        // KEY_EQUAL
    BracketLeft = 26,  // KEY_LEFTBRACE
    BracketRight = 27, // KEY_RIGHTBRACE
    Backslash = 43,    // KEY_BACKSHLASH
    Semicolon = 39,    // KEY_SEMICOLON
    Quote = 40,        // KEY_APOSTROPHE
    Comma = 51,        // KEY_COMMA
    Period = 52,       // KEY_DOT
    Slash = 53,        // KEY_SLASH
    Grave = 41,        // KEY_GRAVE
    IsoExtra = 86,     // KEY_102ND
    IsoHash = 99,      // KEY_HASHTHILDE
}

impl Key {
    #[inline(always)]
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

    /// Convert a `HidUsage` to the Linux-native variant.
    ///
    /// Only Keyboard/Keypad page usages with a native `Key` variant are
    /// resolvable; Consumer Page usages (media keys) and usages without a
    /// `Key` variant (`NumpadClear`, `NumpadEqual`) return `None`.
    pub fn from_hid_usage(usage: HidUsage) -> Option<Self> {
        let code = super::hid_translate::hid_usage_to_keycode(usage)?;
        Self::from_native(code)
    }

    /// Convert the Linux-native variant to its `HidUsage`.
    ///
    /// Every `Key` variant maps to a Keyboard/Keypad page usage, so this
    /// never fails.
    pub fn to_hid_usage(self) -> HidUsage {
        super::hid_translate::keycode_to_hid_usage(self.as_native())
            .expect("all Key variants map to a keyboard page HID usage")
    }

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
        }
    }

    /// All defined key variants.
    pub const ALL: [Self; 100] = [
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
        Self::Numpad7,
        Self::Numpad8,
        Self::Numpad9,
        Self::Numpad4,
        Self::Numpad5,
        Self::Numpad6,
        Self::Numpad1,
        Self::Numpad2,
        Self::Numpad3,
        Self::Numpad0,
        Self::NumpadDecimal,
        Self::NumpadPlus,
        Self::NumpadEnter,
        Self::NumpadMinus,
        Self::NumpadMultiply,
        Self::NumpadDivide,
        // Punctuation
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
    ];

    /// Convert a native evdev key code back to a Key variant.
    ///
    /// Returns `None` for codes that are not defined in this enum.
    pub const fn from_native(code: u16) -> Option<Self> {
        match code {
            29 => Some(Self::LeftControl),
            97 => Some(Self::RightControl),
            42 => Some(Self::LeftShift),
            54 => Some(Self::RightShift),
            56 => Some(Self::LeftAlt),
            100 => Some(Self::RightAlt),
            125 => Some(Self::LeftCommand),
            126 => Some(Self::RightCommand),
            58 => Some(Self::CapsLock),
            15 => Some(Self::Tab),
            57 => Some(Self::Space),
            28 => Some(Self::Return),
            14 => Some(Self::Backspace),
            111 => Some(Self::Delete),
            1 => Some(Self::Escape),
            103 => Some(Self::UpArrow),
            108 => Some(Self::DownArrow),
            105 => Some(Self::LeftArrow),
            106 => Some(Self::RightArrow),
            104 => Some(Self::PageUp),
            109 => Some(Self::PageDown),
            102 => Some(Self::Home),
            107 => Some(Self::End),
            59 => Some(Self::F1),
            60 => Some(Self::F2),
            61 => Some(Self::F3),
            62 => Some(Self::F4),
            63 => Some(Self::F5),
            64 => Some(Self::F6),
            65 => Some(Self::F7),
            66 => Some(Self::F8),
            67 => Some(Self::F9),
            68 => Some(Self::F10),
            87 => Some(Self::F11),
            88 => Some(Self::F12),
            30 => Some(Self::A),
            48 => Some(Self::B),
            46 => Some(Self::C),
            32 => Some(Self::D),
            18 => Some(Self::E),
            33 => Some(Self::F),
            34 => Some(Self::G),
            35 => Some(Self::H),
            23 => Some(Self::I),
            36 => Some(Self::J),
            37 => Some(Self::K),
            38 => Some(Self::L),
            50 => Some(Self::M),
            49 => Some(Self::N),
            24 => Some(Self::O),
            25 => Some(Self::P),
            16 => Some(Self::Q),
            19 => Some(Self::R),
            31 => Some(Self::S),
            20 => Some(Self::T),
            22 => Some(Self::U),
            47 => Some(Self::V),
            17 => Some(Self::W),
            45 => Some(Self::X),
            21 => Some(Self::Y),
            44 => Some(Self::Z),
            2 => Some(Self::Number1),
            3 => Some(Self::Number2),
            4 => Some(Self::Number3),
            5 => Some(Self::Number4),
            6 => Some(Self::Number5),
            7 => Some(Self::Number6),
            8 => Some(Self::Number7),
            9 => Some(Self::Number8),
            10 => Some(Self::Number9),
            11 => Some(Self::Number0),
            71 => Some(Self::Numpad7),
            72 => Some(Self::Numpad8),
            73 => Some(Self::Numpad9),
            75 => Some(Self::Numpad4),
            76 => Some(Self::Numpad5),
            77 => Some(Self::Numpad6),
            79 => Some(Self::Numpad1),
            80 => Some(Self::Numpad2),
            81 => Some(Self::Numpad3),
            82 => Some(Self::Numpad0),
            83 => Some(Self::NumpadDecimal),
            78 => Some(Self::NumpadPlus),
            96 => Some(Self::NumpadEnter),
            74 => Some(Self::NumpadMinus),
            55 => Some(Self::NumpadMultiply),
            98 => Some(Self::NumpadDivide),
            12 => Some(Self::Minus),
            13 => Some(Self::Equal),
            26 => Some(Self::BracketLeft),
            27 => Some(Self::BracketRight),
            43 => Some(Self::Backslash),
            39 => Some(Self::Semicolon),
            40 => Some(Self::Quote),
            51 => Some(Self::Comma),
            52 => Some(Self::Period),
            53 => Some(Self::Slash),
            41 => Some(Self::Grave),
            86 => Some(Self::IsoExtra),
            99 => Some(Self::IsoHash),
            _ => None,
        }
    }

    pub fn try_from_str(name: &str) -> Option<Self> {
        match name {
            "Ctrl" => Some(Self::LeftControl),
            "Shift" => Some(Self::LeftShift),
            "Alt" | "Option" => Some(Self::LeftAlt),
            "Command" | "Cmd" | "Super" => Some(Self::LeftCommand),
            "LeftControl" | "LeftCtrl" => Some(Self::LeftControl),
            "RightControl" | "RightCtrl" => Some(Self::RightControl),
            "LeftShift" => Some(Self::LeftShift),
            "RightShift" => Some(Self::RightShift),
            "LeftAlt" | "LeftOption" => Some(Self::LeftAlt),
            "RightAlt" | "RightOption" => Some(Self::RightAlt),
            "LeftCommand" | "LeftCmd" => Some(Self::LeftCommand),
            "RightCommand" | "RightCmd" => Some(Self::RightCommand),
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
            "NumpadDecimal" | "KP_Decimal" => Some(Self::NumpadDecimal),
            "NumpadMultiply" | "KP_Multiply" => Some(Self::NumpadMultiply),
            "NumpadPlus" | "KP_Add" => Some(Self::NumpadPlus),
            "NumpadDivide" | "KP_Divide" => Some(Self::NumpadDivide),
            "NumpadEnter" | "KP_Enter" => Some(Self::NumpadEnter),
            "NumpadMinus" | "KP_Subtract" => Some(Self::NumpadMinus),
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
