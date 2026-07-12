// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Global abstract key definitions used in the configuration file.
///
/// Accepts case-insensitive string names in TOML (e.g. `"capslock"`,
/// `"CapsLock"`, `"CAPSLOCK"` are all equivalent).  Serialises back to the
/// canonical PascalCase form.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AbstractKey {
    // --- Modifiers ---
    LeftControl,
    RightControl,
    LeftShift,
    RightShift,
    LeftAlt,
    RightAlt,
    LeftCommand,  // macOS Command / Windows Super
    RightCommand, // macOS Command / Windows Super
    CapsLock,
    // --- Editor / misc ---
    Tab,
    Space,
    Return, // Enter / Return
    Backspace,
    Delete,
    Escape,
    // --- Navigation ---
    UpArrow,
    DownArrow,
    LeftArrow,
    RightArrow,
    PageUp,
    PageDown,
    Home,
    End,
    // --- Function keys ---
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    // --- Letters ---
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    // --- Numbers ---
    Number1,
    Number2,
    Number3,
    Number4,
    Number5,
    Number6,
    Number7,
    Number8,
    Number9,
    Number0,
}

impl AbstractKey {
    /// Case-insensitive lookup from a human-readable name.
    /// Accepts the canonical PascalCase name plus common aliases.
    fn from_str(s: &str) -> Result<Self, String> {
        // Normalise: lowercase, then match.
        let l = s.to_lowercase();

        // Try common aliases first.
        match l.as_str() {
            "ctrl" | "leftctrl" | "leftcontrol" => {
                return Ok(Self::LeftControl);
            }
            "rightctrl" | "rightcontrol" => return Ok(Self::RightControl),
            "shift" | "leftshift" => return Ok(Self::LeftShift),
            "rightshift" => return Ok(Self::RightShift),
            "alt" | "leftalt" => return Ok(Self::LeftAlt),
            "rightalt" | "rightoption" => return Ok(Self::RightAlt),
            "cmd" | "command" | "leftcmd" | "leftcommand" | "super"
            | "leftsuper" => return Ok(Self::LeftCommand),
            "rightcmd" | "rightcommand" | "rightsuper" => {
                return Ok(Self::RightCommand);
            }
            "caps" | "capslock" => return Ok(Self::CapsLock),
            "return" | "enter" => return Ok(Self::Return),
            "up" | "uparrow" | "up_arrow" => return Ok(Self::UpArrow),
            "down" | "downarrow" | "down_arrow" => return Ok(Self::DownArrow),
            "left" | "leftarrow" | "left_arrow" => return Ok(Self::LeftArrow),
            "right" | "rightarrow" | "right_arrow" => {
                return Ok(Self::RightArrow);
            }
            "leftwin" | "win" => return Ok(Self::LeftCommand),
            _ => {}
        }

        // Fall back to the canonical name (lowercased).
        Ok(match l.as_str() {
            "tab" => Self::Tab,
            "space" => Self::Space,
            "backspace" => Self::Backspace,
            "delete" => Self::Delete,
            "escape" | "esc" => Self::Escape,
            "pageup" | "page_up" | "pgup" => Self::PageUp,
            "pagedown" | "page_down" | "pgdn" => Self::PageDown,
            "home" => Self::Home,
            "end" => Self::End,
            "f1" => Self::F1,
            "f2" => Self::F2,
            "f3" => Self::F3,
            "f4" => Self::F4,
            "f5" => Self::F5,
            "f6" => Self::F6,
            "f7" => Self::F7,
            "f8" => Self::F8,
            "f9" => Self::F9,
            "f10" => Self::F10,
            "f11" => Self::F11,
            "f12" => Self::F12,
            "a" => Self::A,
            "b" => Self::B,
            "c" => Self::C,
            "d" => Self::D,
            "e" => Self::E,
            "f" => Self::F,
            "g" => Self::G,
            "h" => Self::H,
            "i" => Self::I,
            "j" => Self::J,
            "k" => Self::K,
            "l" => Self::L,
            "m" => Self::M,
            "n" => Self::N,
            "o" => Self::O,
            "p" => Self::P,
            "q" => Self::Q,
            "r" => Self::R,
            "s" => Self::S,
            "t" => Self::T,
            "u" => Self::U,
            "v" => Self::V,
            "w" => Self::W,
            "x" => Self::X,
            "y" => Self::Y,
            "z" => Self::Z,
            "1" | "number1" => Self::Number1,
            "2" | "number2" => Self::Number2,
            "3" | "number3" => Self::Number3,
            "4" | "number4" => Self::Number4,
            "5" | "number5" => Self::Number5,
            "6" | "number6" => Self::Number6,
            "7" | "number7" => Self::Number7,
            "8" | "number8" => Self::Number8,
            "9" | "number9" => Self::Number9,
            "0" | "number0" => Self::Number0,
            _ => {
                return Err(format!(
                    "unknown key name '{}'. Use names like capslock, \
                     leftcontrol, a, f1, 1, etc.",
                    s
                ));
            }
        })
    }

    /// Return the canonical string name for serialisation.
    fn as_str(&self) -> &str {
        match self {
            Self::LeftControl => "leftcontrol",
            Self::RightControl => "rightcontrol",
            Self::LeftShift => "leftshift",
            Self::RightShift => "rightshift",
            Self::LeftAlt => "leftalt",
            Self::RightAlt => "rightalt",
            Self::LeftCommand => "leftcommand",
            Self::RightCommand => "rightcommand",
            Self::CapsLock => "capslock",
            Self::Tab => "tab",
            Self::Space => "space",
            Self::Return => "return",
            Self::Backspace => "backspace",
            Self::Delete => "delete",
            Self::Escape => "escape",
            Self::UpArrow => "uparrow",
            Self::DownArrow => "downarrow",
            Self::LeftArrow => "leftarrow",
            Self::RightArrow => "rightarrow",
            Self::PageUp => "pageup",
            Self::PageDown => "pagedown",
            Self::Home => "home",
            Self::End => "end",
            Self::F1 => "f1",
            Self::F2 => "f2",
            Self::F3 => "f3",
            Self::F4 => "f4",
            Self::F5 => "f5",
            Self::F6 => "f6",
            Self::F7 => "f7",
            Self::F8 => "f8",
            Self::F9 => "f9",
            Self::F10 => "f10",
            Self::F11 => "f11",
            Self::F12 => "f12",
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
            Self::D => "d",
            Self::E => "e",
            Self::F => "f",
            Self::G => "g",
            Self::H => "h",
            Self::I => "i",
            Self::J => "j",
            Self::K => "k",
            Self::L => "l",
            Self::M => "m",
            Self::N => "n",
            Self::O => "o",
            Self::P => "p",
            Self::Q => "q",
            Self::R => "r",
            Self::S => "s",
            Self::T => "t",
            Self::U => "u",
            Self::V => "v",
            Self::W => "w",
            Self::X => "x",
            Self::Y => "y",
            Self::Z => "z",
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
        }
    }
}

impl Serialize for AbstractKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AbstractKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// The type of action to execute when a key condition matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeyAction {
    /// Remap to another single key (e.g., CapsLock -> LeftControl)
    RemapTo(AbstractKey),
    /// Simulate a multi-key macro shortcut (e.g., F1 -> Ctrl + T)
    Shortcut(Vec<AbstractKey>),
}

/// A specific mapping rule bound to applications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingRule {
    pub description: Option<String>,
    /// List of target applications (process names or bundle IDs).
    /// Empty list means the rule applies globally.
    pub applications: Vec<String>,
    pub trigger: AbstractKey,
    pub action: KeyAction,
}

/// The root configuration layout representing the file structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub rules: Vec<MappingRule>,
}

impl AppConfig {
    pub fn load_from_str(toml_str: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_str)
    }
}
