// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventSource, CGEventSourceStateID,
    CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CGKeyCode,
};
use parking_lot::RwLock;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{mapping_cache::NativeKey, state::Lookup};

// ---------------------------------------------------------------------------
// Platform-specific Key enum — discriminants ARE the CGKeyCode values
// ---------------------------------------------------------------------------

/// macOS virtual keycode for a physical key on a US ANSI keyboard.
///
/// Discriminant values come from `<HIToolbox/Events.h>` (`kVK_*` constants).
/// `key as u16` yields the native CGKeyCode — no translation needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
pub enum Key {
    // --- Modifiers ---
    LeftControl = 59,  // kVK_Control
    RightControl = 62, // kVK_RightControl
    LeftShift = 56,    // kVK_Shift
    RightShift = 60,   // kVK_RightShift
    LeftAlt = 58,      // kVK_Option
    RightAlt = 61,     // kVK_RightOption
    LeftCommand = 55,  // kVK_Command
    RightCommand = 54, // kVK_RightCommand
    CapsLock = 57,     // kVK_CapsLock
    // --- Editor / misc ---
    Tab = 48,       // kVK_Tab
    Space = 49,     // kVK_Space
    Return = 36,    // kVK_Return
    Backspace = 51, // kVK_Delete
    Delete = 117,   // kVK_ForwardDelete
    Escape = 53,    // kVK_Escape
    // --- Navigation ---
    UpArrow = 126,    // kVK_UpArrow
    DownArrow = 125,  // kVK_DownArrow
    LeftArrow = 123,  // kVK_LeftArrow
    RightArrow = 124, // kVK_RightArrow
    PageUp = 116,     // kVK_PageUp
    PageDown = 121,   // kVK_PageDown
    Home = 115,       // kVK_Home
    End = 119,        // kVK_End
    // --- Function keys ---
    F1 = 122,  // kVK_F1
    F2 = 120,  // kVK_F2
    F3 = 99,   // kVK_F3
    F4 = 118,  // kVK_F4
    F5 = 96,   // kVK_F5
    F6 = 97,   // kVK_F6
    F7 = 98,   // kVK_F7
    F8 = 100,  // kVK_F8
    F9 = 101,  // kVK_F9
    F10 = 109, // kVK_F10
    F11 = 103, // kVK_F11
    F12 = 111, // kVK_F12
    // --- Letters ---
    A = 0,  // kVK_ANSI_A
    B = 11, // kVK_ANSI_B
    C = 8,  // kVK_ANSI_C
    D = 2,  // kVK_ANSI_D
    E = 14, // kVK_ANSI_E
    F = 3,  // kVK_ANSI_F
    G = 5,  // kVK_ANSI_G
    H = 4,  // kVK_ANSI_H
    I = 34, // kVK_ANSI_I
    J = 38, // kVK_ANSI_J
    K = 40, // kVK_ANSI_K
    L = 37, // kVK_ANSI_L
    M = 46, // kVK_ANSI_M
    N = 45, // kVK_ANSI_N
    O = 31, // kVK_ANSI_O
    P = 35, // kVK_ANSI_P
    Q = 12, // kVK_ANSI_Q
    R = 15, // kVK_ANSI_R
    S = 1,  // kVK_ANSI_S
    T = 17, // kVK_ANSI_T
    U = 32, // kVK_ANSI_U
    V = 9,  // kVK_ANSI_V
    W = 13, // kVK_ANSI_W
    X = 7,  // kVK_ANSI_X
    Y = 16, // kVK_ANSI_Y
    Z = 6,  // kVK_ANSI_Z
    // --- Numbers ---
    Number1 = 18, // kVK_ANSI_1
    Number2 = 19, // kVK_ANSI_2
    Number3 = 20, // kVK_ANSI_3
    Number4 = 21, // kVK_ANSI_4
    Number5 = 23, // kVK_ANSI_5
    Number6 = 22, // kVK_ANSI_6
    Number7 = 26, // kVK_ANSI_7
    Number8 = 28, // kVK_ANSI_8
    Number9 = 25, // kVK_ANSI_9
    Number0 = 29, // kVK_ANSI_0
    // --- Numpad ---
    Numpad0 = 82,        // kVK_ANSI_Keypad0
    Numpad1 = 83,        // kVK_ANSI_Keypad1
    Numpad2 = 84,        // kVK_ANSI_Keypad2
    Numpad3 = 85,        // kVK_ANSI_Keypad3
    Numpad4 = 86,        // kVK_ANSI_Keypad4
    Numpad5 = 87,        // kVK_ANSI_Keypad5
    Numpad6 = 88,        // kVK_ANSI_Keypad6
    Numpad7 = 89,        // kVK_ANSI_Keypad7
    Numpad8 = 91,        // kVK_ANSI_Keypad8
    Numpad9 = 92,        // kVK_ANSI_Keypad9
    NumpadDecimal = 65,  // kVK_ANSI_KeypadDecimal
    NumpadMultiply = 75, // kVK_ANSI_KeypadMultiply
    NumpadPlus = 69,     // kVK_ANSI_KeypadPlus
    NumpadClear = 71,    // kVK_ANSI_KeypadClear
    NumpadDivide = 73,   // kVK_ANSI_KeypadDivide
    NumpadEnter = 76,    // kVK_ANSI_KeypadEnter
    NumpadMinus = 78,    // kVK_ANSI_KeypadMinus
    NumpadEqual = 90,    // kVK_ANSI_KeypadEqual
    // --- Punctuation / symbols ---
    Minus = 27,        // kVK_ANSI_Minus
    Equal = 24,        // kVK_ANSI_Equal
    BracketLeft = 33,  // kVK_ANSI_LeftBracket
    BracketRight = 30, // kVK_ANSI_RightBracket
    Backslash = 42,    // kVK_ANSI_Backslash
    Semicolon = 41,    // kVK_ANSI_Semicolon
    Quote = 39,        // kVK_ANSI_Quote
    Comma = 43,        // kVK_ANSI_Comma
    Period = 47,       // kVK_ANSI_Period
    Slash = 44,        // kVK_ANSI_Slash
    Grave = 50,        // kVK_ANSI_Grave
    IsoExtra = 10,     // kVK_ISO_Section (between Shift and Z on ISO)
}

impl Key {
    /// Convert to the native CGKeyCode.  Zero-cost — the discriminant IS the
    /// code.
    pub const fn as_native(self) -> u16 {
        self as u16
    }

    /// Return the modifier bit **position** (0–7) for this key.
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

    /// Return the possible modifier bit positions for this key.
    ///
    /// Modifier keys return both left and right positions, enabling
    /// "either side" matching.  Non-modifier keys return `None`.
    pub fn as_modifier_positions(self) -> Option<Vec<u8>> {
        match self {
            Self::LeftControl | Self::RightControl => Some(vec![0, 1]),
            Self::LeftShift | Self::RightShift => Some(vec![2, 3]),
            Self::LeftAlt | Self::RightAlt => Some(vec![4, 5]),
            Self::LeftCommand | Self::RightCommand => Some(vec![6, 7]),
            _ => None,
        }
    }

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
            Self::NumpadClear => "NumpadClear",
            Self::NumpadDivide => "NumpadDivide",
            Self::NumpadEnter => "NumpadEnter",
            Self::NumpadMinus => "NumpadMinus",
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
        }
    }

    /// All defined key variants.
    pub const ALL: [Self; 101] = [
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
        Self::NumpadClear,
        Self::NumpadDivide,
        Self::NumpadEnter,
        Self::NumpadMinus,
        Self::NumpadEqual,
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
    ];

    /// Convert a native CGKeyCode back to a Key variant.
    ///
    /// Returns `None` for codes that are not defined in this enum.
    pub const fn from_native(code: u16) -> Option<Self> {
        match code {
            59 => Some(Self::LeftControl),
            62 => Some(Self::RightControl),
            56 => Some(Self::LeftShift),
            60 => Some(Self::RightShift),
            58 => Some(Self::LeftAlt),
            61 => Some(Self::RightAlt),
            55 => Some(Self::LeftCommand),
            54 => Some(Self::RightCommand),
            57 => Some(Self::CapsLock),
            48 => Some(Self::Tab),
            49 => Some(Self::Space),
            36 => Some(Self::Return),
            51 => Some(Self::Backspace),
            117 => Some(Self::Delete),
            53 => Some(Self::Escape),
            126 => Some(Self::UpArrow),
            125 => Some(Self::DownArrow),
            123 => Some(Self::LeftArrow),
            124 => Some(Self::RightArrow),
            116 => Some(Self::PageUp),
            121 => Some(Self::PageDown),
            115 => Some(Self::Home),
            119 => Some(Self::End),
            122 => Some(Self::F1),
            120 => Some(Self::F2),
            99 => Some(Self::F3),
            118 => Some(Self::F4),
            96 => Some(Self::F5),
            97 => Some(Self::F6),
            98 => Some(Self::F7),
            100 => Some(Self::F8),
            101 => Some(Self::F9),
            109 => Some(Self::F10),
            103 => Some(Self::F11),
            111 => Some(Self::F12),
            0 => Some(Self::A),
            11 => Some(Self::B),
            8 => Some(Self::C),
            2 => Some(Self::D),
            14 => Some(Self::E),
            3 => Some(Self::F),
            5 => Some(Self::G),
            4 => Some(Self::H),
            34 => Some(Self::I),
            38 => Some(Self::J),
            40 => Some(Self::K),
            37 => Some(Self::L),
            46 => Some(Self::M),
            45 => Some(Self::N),
            31 => Some(Self::O),
            35 => Some(Self::P),
            12 => Some(Self::Q),
            15 => Some(Self::R),
            1 => Some(Self::S),
            17 => Some(Self::T),
            32 => Some(Self::U),
            9 => Some(Self::V),
            13 => Some(Self::W),
            7 => Some(Self::X),
            16 => Some(Self::Y),
            6 => Some(Self::Z),
            18 => Some(Self::Number1),
            19 => Some(Self::Number2),
            20 => Some(Self::Number3),
            21 => Some(Self::Number4),
            23 => Some(Self::Number5),
            22 => Some(Self::Number6),
            26 => Some(Self::Number7),
            28 => Some(Self::Number8),
            25 => Some(Self::Number9),
            29 => Some(Self::Number0),
            82 => Some(Self::Numpad0),
            83 => Some(Self::Numpad1),
            84 => Some(Self::Numpad2),
            85 => Some(Self::Numpad3),
            86 => Some(Self::Numpad4),
            87 => Some(Self::Numpad5),
            88 => Some(Self::Numpad6),
            89 => Some(Self::Numpad7),
            91 => Some(Self::Numpad8),
            92 => Some(Self::Numpad9),
            65 => Some(Self::NumpadDecimal),
            75 => Some(Self::NumpadMultiply),
            69 => Some(Self::NumpadPlus),
            71 => Some(Self::NumpadClear),
            73 => Some(Self::NumpadDivide),
            76 => Some(Self::NumpadEnter),
            78 => Some(Self::NumpadMinus),
            90 => Some(Self::NumpadEqual),
            27 => Some(Self::Minus),
            24 => Some(Self::Equal),
            33 => Some(Self::BracketLeft),
            30 => Some(Self::BracketRight),
            42 => Some(Self::Backslash),
            41 => Some(Self::Semicolon),
            39 => Some(Self::Quote),
            43 => Some(Self::Comma),
            47 => Some(Self::Period),
            44 => Some(Self::Slash),
            50 => Some(Self::Grave),
            10 => Some(Self::IsoExtra),
            _ => None,
        }
    }

    /// Parse a TitleCase key name into a Key variant.
    ///
    /// Case-sensitive matching.  Generic modifier names (`Ctrl`, `Shift`,
    /// `Alt`, `Command`) resolve to left-side defaults.  Explicit names
    /// (`LeftControl`, `RightAlt`) are preserved.
    pub fn try_from_str(name: &str) -> Option<Self> {
        match name {
            // Generic modifiers — resolve to left-side defaults
            "Ctrl" => Some(Self::LeftControl),
            "Shift" => Some(Self::LeftShift),
            "Alt" | "Option" => Some(Self::LeftAlt),
            "Command" | "Cmd" | "Super" => Some(Self::LeftCommand),
            // Specific modifiers
            "LeftControl" | "LeftCtrl" => Some(Self::LeftControl),
            "RightControl" | "RightCtrl" => Some(Self::RightControl),
            "LeftShift" => Some(Self::LeftShift),
            "RightShift" => Some(Self::RightShift),
            "LeftAlt" | "LeftOption" => Some(Self::LeftAlt),
            "RightAlt" | "RightOption" => Some(Self::RightAlt),
            "LeftCommand" | "LeftCmd" => Some(Self::LeftCommand),
            "RightCommand" | "RightCmd" => Some(Self::RightCommand),
            // Non-modifier keys
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
            "NumpadMultiply" | "KP_Multiply" => Some(Self::NumpadMultiply),
            "NumpadPlus" | "KP_Add" => Some(Self::NumpadPlus),
            "NumpadClear" => Some(Self::NumpadClear),
            "NumpadDivide" | "KP_Divide" => Some(Self::NumpadDivide),
            "NumpadEnter" | "KP_Enter" => Some(Self::NumpadEnter),
            "NumpadMinus" | "KP_Subtract" => Some(Self::NumpadMinus),
            "NumpadEqual" => Some(Self::NumpadEqual),
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
            "IsoExtra" | "NonUSBackslash" => Some(Self::IsoExtra),
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
// Modifier handling — track specific key state for exact matching
// ---------------------------------------------------------------------------

/// Map a CGKeyCode to its modifier bit position.  Returns `None` for
/// non-modifier keys.
fn keycode_to_modifier_bit(code: CGKeyCode) -> Option<u8> {
    match code {
        59 => Some(0), // kVK_Control (left)
        62 => Some(1), // kVK_RightControl
        56 => Some(2), // kVK_Shift (left)
        60 => Some(3), // kVK_RightShift
        58 => Some(4), // kVK_Option (left)
        61 => Some(5), // kVK_RightOption
        55 => Some(6), // kVK_Command (left)
        54 => Some(7), // kVK_RightCommand
        _ => None,
    }
}

/// Map a modifier bit position back to the native CGKeyCode for emission.
fn modifier_bit_to_code(bit: u8) -> Option<CGKeyCode> {
    match bit {
        0 => Some(59), // kVK_Control (left)
        1 => Some(62), // kVK_RightControl
        2 => Some(56), // kVK_Shift (left)
        3 => Some(60), // kVK_RightShift
        4 => Some(58), // kVK_Option (left)
        5 => Some(61), // kVK_RightOption
        6 => Some(55), // kVK_Command (left)
        7 => Some(54), // kVK_RightCommand
        _ => None,
    }
}

/// Emit a single `NativeKey` as a chord: hold modifiers, press base,
/// release base, release modifiers in reverse order.
fn emit_key_event(source: &CFRetained<CGEventSource>, native_key: &NativeKey) {
    let mut pressed_modifiers: Vec<CGKeyCode> = Vec::new();

    // Press modifiers.
    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1
            && let Some(code) = modifier_bit_to_code(bit)
        {
            if let Some(e) =
                CGEvent::new_keyboard_event(Some(source), code, true)
            {
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
            }
            pressed_modifiers.push(code);
            thread::sleep(Duration::from_millis(1));
        }
    }

    // Press base key.
    if let Some(e) = CGEvent::new_keyboard_event(
        Some(source),
        native_key.base as CGKeyCode,
        true,
    ) {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
    }
    thread::sleep(Duration::from_millis(1));

    // Release base key.
    if let Some(e) = CGEvent::new_keyboard_event(
        Some(source),
        native_key.base as CGKeyCode,
        false,
    ) {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
    }
    thread::sleep(Duration::from_millis(1));

    // Release modifiers.
    for code in pressed_modifiers.into_iter() {
        if let Some(e) = CGEvent::new_keyboard_event(Some(source), code, false)
        {
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

// ---------------------------------------------------------------------------
// Event tap implementation
// ---------------------------------------------------------------------------

/// Shared mutable state bridged into the C callback via `user_info`.
struct TapContext {
    /// Trait-object lookup: decouples this module from RuntimeState's shape.
    lookup: Arc<RwLock<dyn Lookup>>,
    /// Pre-created event source reused for every synthetic keyboard event.
    /// Avoids a per-keystroke allocation inside the hot callback path.
    source: CFRetained<CGEventSource>,
    /// Bitmask tracking which specific modifier keys are physically pressed.
    modifier_state: u8,
}

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Holds the tap, run-loop-source, and callback context so they stay alive
/// for the lifetime of the event-loop, and are cleanly reclaimed on drop.
struct EventTapHandle {
    tap: CFRetained<CFMachPort>,
    #[allow(dead_code)]
    run_loop_source: CFRetained<CFRunLoopSource>,
    /// Raw pointer to the heap-allocated `TapContext` passed as `user_info`.
    context_ptr: *mut TapContext,
}

impl Drop for EventTapHandle {
    fn drop(&mut self) {
        CGEvent::tap_enable(&self.tap, false);
        unsafe {
            drop(Box::from_raw(self.context_ptr));
        }
    }
}

/// Async-signal-safe handler that flips the shutdown flag.
extern "C" fn signal_handler(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mask: u64 =
        (1u64 << CGEventType::KeyDown.0) | (1u64 << CGEventType::KeyUp.0);

    let source =
        CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .ok_or("Failed to create CGEventSource")?;

    let context_ptr = Box::into_raw(Box::new(TapContext {
        lookup,
        source,
        modifier_state: 0,
    })) as *mut _;

    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(macos_keyboard_callback_ffi),
            context_ptr as *mut c_void,
        )
    };

    let Some(tap) = tap else {
        unsafe {
            drop(Box::from_raw(context_ptr));
        }
        return Err("Failed to create macOS CGEventTap. Verify \
                    Accessibility privileges!"
            .into());
    };

    let Some(run_loop_source) =
        CFMachPort::new_run_loop_source(None, Some(&tap), 0)
    else {
        unsafe {
            drop(Box::from_raw(context_ptr));
        }
        return Err("Failed to create CFRunLoopSource from Mach Port.".into());
    };

    let run_loop = CFRunLoop::current().ok_or("No current run loop")?;
    run_loop
        .add_source(Some(&run_loop_source), unsafe { kCFRunLoopCommonModes });

    CGEvent::tap_enable(&tap, true);
    println!("macOS Event Tap running.");

    let handler_ptr = signal_handler as *const () as usize;
    unsafe {
        libc::signal(libc::SIGINT, handler_ptr);
        libc::signal(libc::SIGTERM, handler_ptr);
    }

    let handle = EventTapHandle {
        tap,
        run_loop_source,
        context_ptr,
    };

    while !SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopCommonModes }, 0.5, true);
    }

    println!("Shutdown signal received. Cleaning up...");
    drop(handle);

    Ok(())
}

/// FFI callback invoked by the event tap for every matching keyboard event.
unsafe extern "C-unwind" fn macos_keyboard_callback_ffi(
    _proxy: objc2_core_graphics::CGEventTapProxy,
    _type: CGEventType,
    event: core::ptr::NonNull<objc2_core_graphics::CGEvent>,
    user_info: *mut std::ffi::c_void,
) -> *mut objc2_core_graphics::CGEvent {
    if user_info.is_null() {
        return event.as_ptr();
    }

    let context = unsafe { &mut *(user_info as *mut TapContext) };

    let native_key: CGKeyCode = unsafe {
        CGEvent::integer_value_field(
            Some(event.as_ref()),
            CGEventField::KeyboardEventKeycode,
        )
    } as CGKeyCode;

    let is_down = _type == CGEventType::KeyDown;

    // Track specific modifier key state for exact matching.
    if let Some(bit) = keycode_to_modifier_bit(native_key) {
        if is_down {
            context.modifier_state |= 1 << bit;
        } else {
            context.modifier_state &= !(1 << bit);
        }
        return event.as_ptr();
    }

    let pressed_modifiers = context.modifier_state;

    let guard = context.lookup.read();
    let current_app = guard.active_app().to_string();
    let active_outputs = guard
        .for_app(&current_app, native_key, pressed_modifiers)
        .or_else(|| guard.global(native_key, pressed_modifiers))
        .map(|v| v.to_vec());
    drop(guard);

    if let Some(outputs) = active_outputs {
        if is_down {
            for native_key in &outputs {
                emit_key_event(&context.source, native_key);
            }
        }
        return std::ptr::null_mut();
    }

    event.as_ptr()
}
