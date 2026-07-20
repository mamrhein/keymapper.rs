// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    ptr::null_mut,
    sync::{Arc, OnceLock},
    thread,
    time::Duration,
};

use parking_lot::RwLock;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use windows_sys::Windows::Win32::{
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
            KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, HHOOK, HINSTANCE, KBDLLHOOKSTRUCT,
            LPARAM, LRESULT, MSG, SetWindowsHookExW, UnhookWindowsHookEx,
            WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
            WPARAM,
        },
    },
};

use crate::{mapping_cache::NativeKey, state::Lookup};

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
}

impl Key {
    pub const fn as_native(self) -> VIRTUAL_KEY {
        self as VIRTUAL_KEY
    }

    pub const fn as_modifier_bit(self) -> Option<u8> {
        match self {
            Self::LeftControl => Some(0),
            Self::RightControl => Some(1),
            Self::LeftShift => Some(2),
            Self::RightShift => Some(3),
            Self::LeftAlt => Some(4),
            Self::RightAlt => Some(5),
            Self::LeftCommand => Some(6),
            Self::RightCommand => Some(7),
            _ => None,
        }
    }

    pub fn as_modifier_positions(self) -> Option<Vec<u8>> {
        match self {
            Self::LeftControl | Self::RightControl => Some(vec![0, 1]),
            Self::LeftShift | Self::RightShift => Some(vec![2, 3]),
            Self::LeftAlt | Self::RightAlt => Some(vec![4, 5]),
            Self::LeftCommand | Self::RightCommand => Some(vec![6, 7]),
            _ => None,
        }
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
            serde::de::Error::custom(crate::key_names::unknown_key_error(&s))
        })
    }
}

// ---------------------------------------------------------------------------
// Modifier handling
// ---------------------------------------------------------------------------

fn extract_modifier_bits() -> u8 {
    let mut bits: u8 = 0;
    if unsafe { GetAsyncKeyState(0xA2) } < 0 {
        bits |= 1 << 0;
    }
    if unsafe { GetAsyncKeyState(0xA3) } < 0 {
        bits |= 1 << 1;
    }
    if unsafe { GetAsyncKeyState(0xA0) } < 0 {
        bits |= 1 << 2;
    }
    if unsafe { GetAsyncKeyState(0xA1) } < 0 {
        bits |= 1 << 3;
    }
    if unsafe { GetAsyncKeyState(0xA4) } < 0 {
        bits |= 1 << 4;
    }
    if unsafe { GetAsyncKeyState(0xA5) } < 0 {
        bits |= 1 << 5;
    }
    if unsafe { GetAsyncKeyState(0x5B) } < 0 {
        bits |= 1 << 6;
    }
    if unsafe { GetAsyncKeyState(0x5C) } < 0 {
        bits |= 1 << 7;
    }
    bits
}

fn modifier_bit_to_vk(bit: u8) -> Option<VIRTUAL_KEY> {
    match bit {
        0 => Some(0xA2),
        1 => Some(0xA3),
        2 => Some(0xA0),
        3 => Some(0xA1),
        4 => Some(0xA4),
        5 => Some(0xA5),
        6 => Some(0x5B),
        7 => Some(0x5C),
        _ => None,
    }
}

fn is_extended_key(vk: VIRTUAL_KEY) -> bool {
    matches!(
        vk,
        0xA3 | 0xA5 | 0x21 | 0x22 | 0x23 | 0x25
            ..=0x28 | 0x2D | 0x2E | 0x6F | 0x92
    )
}

fn simulate_key_event(vk: VIRTUAL_KEY, is_key_up: bool) {
    let mut flags = if is_key_up { KEYEVENTF_KEYUP } else { 0 };
    if is_extended_key(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }

    let mut input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(
            1,
            std::ptr::addr_of!(input),
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}

fn emit_key_event(native_key: &NativeKey) {
    let mut pressed_modifiers: Vec<VIRTUAL_KEY> = Vec::new();

    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1 {
            if let Some(vk) = modifier_bit_to_vk(bit) {
                simulate_key_event(vk, false);
                pressed_modifiers.push(vk);
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    simulate_key_event(native_key.base as VIRTUAL_KEY, false);
    thread::sleep(Duration::from_millis(1));

    simulate_key_event(native_key.base as VIRTUAL_KEY, true);
    thread::sleep(Duration::from_millis(1));

    for vk in pressed_modifiers.into_iter().rev() {
        simulate_key_event(vk, true);
        thread::sleep(Duration::from_millis(1));
    }
}

fn vk_to_modifier_bit(vk: VIRTUAL_KEY) -> Option<u8> {
    match vk {
        0xA2 => Some(0),
        0xA3 => Some(1),
        0xA0 => Some(2),
        0xA1 => Some(3),
        0xA4 => Some(4),
        0xA5 => Some(5),
        0x5B => Some(6),
        0x5C => Some(7),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Low-level keyboard hook
// ---------------------------------------------------------------------------

static SHARED_LOOKUP: OnceLock<Arc<RwLock<dyn Lookup>>> = OnceLock::new();
static HOOK_HANDLE: OnceLock<HHOOK> = OnceLock::new();

fn set_shared_lookup(lookup: Arc<RwLock<dyn Lookup>>) {
    SHARED_LOOKUP
        .set(lookup)
        .expect("shared lookup already initialised");
}

fn set_hook_handle(handle: HHOOK) {
    HOOK_HANDLE
        .set(handle)
        .expect("hook handle already initialised");
}

fn hook_handle() -> HHOOK {
    *HOOK_HANDLE.get().expect("hook handle not initialised")
}

pub(crate) fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    set_shared_lookup(lookup);

    let h_instance: HINSTANCE = unsafe { GetModuleHandleW(null_mut()) };

    let handle: HHOOK = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(low_level_keyboard_proc),
            h_instance,
            0,
        )
    };

    if handle == 0 {
        return Err("Failed to install global keyboard hook".into());
    }
    set_hook_handle(handle);
    println!("Windows low-level hook listening.");

    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {}
        UnhookWindowsHookEx(hook_handle());
    }

    Ok(())
}

extern "system" fn low_level_keyboard_proc(
    code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if code < 0 {
        return unsafe {
            CallNextHookEx(hook_handle(), code, w_param, l_param)
        };
    }

    let Some(lookup) = SHARED_LOOKUP.get() else {
        return unsafe {
            CallNextHookEx(hook_handle(), code, w_param, l_param)
        };
    };

    let kbd_struct = unsafe { *(l_param as *const KBDLLHOOKSTRUCT) };
    let vk_code = kbd_struct.vkCode as VIRTUAL_KEY;

    let is_key_down =
        w_param as u32 == WM_KEYDOWN || w_param as u32 == WM_SYSKEYDOWN;
    let _is_key_up =
        w_param as u32 == WM_KEYUP || w_param as u32 == WM_SYSKEYUP;

    // Pass through modifier-only events.
    if vk_to_modifier_bit(vk_code).is_some() {
        return unsafe {
            CallNextHookEx(hook_handle(), code, w_param, l_param)
        };
    }

    let pressed_modifiers = extract_modifier_bits();

    let guard = lookup.read();
    let current_app = guard.active_app().to_string();
    let active_outputs = guard
        .for_app(&current_app, vk_code, pressed_modifiers)
        .or_else(|| guard.global(vk_code, pressed_modifiers))
        .map(|v| v.to_vec());
    drop(guard);

    if let Some(outputs) = active_outputs {
        if is_key_down {
            for native_key in &outputs {
                emit_key_event(native_key);
            }
        }
        return 1; // Swallow the original key
    }

    unsafe { CallNextHookEx(hook_handle(), code, w_param, l_param) }
}
