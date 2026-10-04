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
//!
//! The enum and every table-driven impl (`from_code`, `as_str`, `ALL` and
//! the string parser) are generated from the single declarative table at the
//! bottom of this module.  That table is the one source of truth: adding a
//! key means adding one line, and nothing else can drift out of sync with
//! it.  Platform-specific key code mappings (e.g. the Linux evdev `KEY_*`
//! codes) live in the corresponding `platform::<os>::keycode` module.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::common::modifier::ModifierRole;

/// HID usage page for Keyboard/Keypad.
pub const PAGE_KEYBOARD: u16 = 0x07;

/// HID usage page for Consumer.
pub const PAGE_CONSUMER: u16 = 0x0C;

/// Returns a user-friendly error message for an unrecognised key name.
///
/// Shared by the `HidUsage` parser and the platform `Key` enums, which all
/// resolve the same set of config-facing key names.  The offending token is
/// embedded debug-quoted (rather than raw between quotes) so control
/// characters in a config string — newlines, ESC sequences — are escaped
/// instead of being emitted verbatim into daemon logs or terminal output,
/// where they could forge log lines.
pub(crate) fn unknown_key_error(s: &str) -> String {
    format!(
        "Unknown key name {s:?}. Use names like CapsLock, LeftCtrl, A, F1, \
         1, Minus, Equal, BracketLeft, etc."
    )
}

/// Error returned when a string cannot be parsed as a `HidUsage`.
///
/// The inner string is the formatted message produced by
/// `unknown_key_error`, so the wording lives in exactly one place.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct HidUsageParseError(String);

// ---------------------------------------------------------------------------
// Table-driven generation
// ---------------------------------------------------------------------------
//
// `define_hid_usage!` is the single source of truth for the `HidUsage` enum.
// Each table line declares one variant:
//
//     Variant = 0xPPPPUU, "CanonicalName" [, [alias, ...]];
//
// where `0xPPPPUU` is the combined HID usage `(page << 16) | id`,
// `CanonicalName` is the config-facing string (and serialization form), and
// the optional bracket list holds additional parse aliases.  Platform key
// code mappings (e.g. the Linux evdev `KEY_*` codes) live in the respective
// `platform::<os>::keycode` module.  The macro expands to the enum plus
// every impl that is a pure function of this table, so the variant list is
// written exactly once.

macro_rules! define_hid_usage {
    {
        $(
            $variant:ident = $code:literal, $name:literal
            $(, [ $($alias:literal),* ])?
            ;
        )*
    } => {
        /// HID-based key identity for configuration and cross-platform code.
        ///
        /// The discriminant encodes the combined HID usage as `(page << 16) | id`.
        /// This matches the format of Linux MSC_SCAN codes, allowing direct
        /// conversion via `from_code()`.
        #[repr(u32)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum HidUsage {
            $( $variant = $code, )*
        }

        impl HidUsage {
            /// Construct a `HidUsage` from a combined HID usage code.
            ///
            /// Returns `None` if the code does not match a recognized usage.
            #[inline]
            pub fn from_code(code: u32) -> Option<Self> {
                match code {
                    $( $code => Some(Self::$variant), )*
                    _ => None,
                }
            }

            /// Return the canonical config-name for this key.
            pub fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $name, )*
                }
            }

            /// All defined `HidUsage` variants.
            pub fn all() -> &'static [Self] {
                Self::ALL
            }

            /// Slice of all defined `HidUsage` variants.
            pub const ALL: &[Self] = &[ $( Self::$variant, )* ];
        }

        /// Parse a `HidUsage` from a string slice.
        ///
        /// Accepts canonical names (`LeftControl`, `A`, `F1`) and common
        /// aliases (`Ctrl`, `Cmd`, `Esc`).  Generic modifier names resolve to
        /// left-side defaults.  Case-sensitive.
        impl TryFrom<&str> for HidUsage {
            type Error = HidUsageParseError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                match value {
                    // The extra `?` group keeps `$alias` repeating at the same
                    // depth as in the matcher, which `macro_rules` requires.
                    $( $name $( $( | $alias )* )? => Ok(Self::$variant), )*
                    other => Err(HidUsageParseError(unknown_key_error(other))),
                }
            }
        }
    };
}

define_hid_usage! {
    // --- Keyboard page (0x07) — Modifiers ---
    LeftControl = 0x0700E0, "LeftControl", ["Ctrl", "LeftCtrl"];
    RightControl = 0x0700E4, "RightControl", ["RightCtrl"];
    LeftShift = 0x0700E1, "LeftShift", ["Shift"];
    RightShift = 0x0700E5, "RightShift";
    LeftAlt = 0x0700E2, "LeftAlt", ["Alt", "Option", "LeftOption"];
    RightAlt = 0x0700E6, "RightAlt", ["RightOption"];
    LeftCommand = 0x0700E3, "LeftCommand", ["Command", "Cmd", "Super", "LeftCmd"];
    RightCommand = 0x0700E7, "RightCommand", ["RightCmd"];
    // --- Keyboard page — Caps Lock ---
    CapsLock = 0x070039, "CapsLock", ["Caps"];
    // --- Keyboard page — Editor / misc ---
    Tab = 0x07002B, "Tab";
    Space = 0x07002C, "Space";
    Return = 0x070028, "Return", ["Enter"];
    Backspace = 0x07002A, "Backspace";
    Delete = 0x07004C, "Delete";
    Escape = 0x070029, "Escape", ["Esc"];
    // --- Keyboard page — Navigation ---
    UpArrow = 0x070052, "UpArrow", ["Up"];
    DownArrow = 0x070051, "DownArrow", ["Down"];
    LeftArrow = 0x070050, "LeftArrow", ["Left"];
    RightArrow = 0x07004F, "RightArrow", ["Right"];
    PageUp = 0x07004B, "PageUp", ["PgUp"];
    PageDown = 0x07004E, "PageDown", ["PgDn"];
    Home = 0x07004A, "Home";
    End = 0x07004D, "End";
    // --- Keyboard page — Control/function cluster ---
    PrintScreen = 0x070046, "PrintScreen", ["PrtSc", "SysRq"];
    ScrollLock = 0x070047, "ScrollLock", ["Scroll"];
    Pause = 0x070048, "Pause", ["Break"];
    Insert = 0x070049, "Insert", ["Ins"];
    // --- Keyboard page — Application menu ---
    Menu = 0x070065, "Menu", ["ContextMenu", "Application"];
    // --- Keyboard page — Function keys ---
    F1 = 0x07003A, "F1";
    F2 = 0x07003B, "F2";
    F3 = 0x07003C, "F3";
    F4 = 0x07003D, "F4";
    F5 = 0x07003E, "F5";
    F6 = 0x07003F, "F6";
    F7 = 0x070040, "F7";
    F8 = 0x070041, "F8";
    F9 = 0x070042, "F9";
    F10 = 0x070043, "F10";
    F11 = 0x070044, "F11";
    F12 = 0x070045, "F12";
    // --- Keyboard page — Letters ---
    A = 0x070004, "A";
    B = 0x070005, "B";
    C = 0x070006, "C";
    D = 0x070007, "D";
    E = 0x070008, "E";
    F = 0x070009, "F";
    G = 0x07000A, "G";
    H = 0x07000B, "H";
    I = 0x07000C, "I";
    J = 0x07000D, "J";
    K = 0x07000E, "K";
    L = 0x07000F, "L";
    M = 0x070010, "M";
    N = 0x070011, "N";
    O = 0x070012, "O";
    P = 0x070013, "P";
    Q = 0x070014, "Q";
    R = 0x070015, "R";
    S = 0x070016, "S";
    T = 0x070017, "T";
    U = 0x070018, "U";
    V = 0x070019, "V";
    W = 0x07001A, "W";
    X = 0x07001B, "X";
    Y = 0x07001C, "Y";
    Z = 0x07001D, "Z";
    // --- Keyboard page — Numbers ---
    Number1 = 0x07001E, "1", ["Number1"];
    Number2 = 0x07001F, "2", ["Number2"];
    Number3 = 0x070020, "3", ["Number3"];
    Number4 = 0x070021, "4", ["Number4"];
    Number5 = 0x070022, "5", ["Number5"];
    Number6 = 0x070023, "6", ["Number6"];
    Number7 = 0x070024, "7", ["Number7"];
    Number8 = 0x070025, "8", ["Number8"];
    Number9 = 0x070026, "9", ["Number9"];
    Number0 = 0x070027, "0", ["Number0"];
    // --- Keyboard page — Numpad ---
    Numpad0 = 0x070062, "Numpad0";
    Numpad1 = 0x070059, "Numpad1";
    Numpad2 = 0x07005A, "Numpad2";
    Numpad3 = 0x07005B, "Numpad3";
    Numpad4 = 0x07005C, "Numpad4";
    Numpad5 = 0x07005D, "Numpad5";
    Numpad6 = 0x07005E, "Numpad6";
    Numpad7 = 0x07005F, "Numpad7";
    Numpad8 = 0x070060, "Numpad8";
    Numpad9 = 0x070061, "Numpad9";
    NumpadDecimal = 0x070063, "NumpadDecimal";
    NumpadMultiply = 0x070055, "NumpadMultiply", ["KP_Multiply"];
    NumpadPlus = 0x070057, "NumpadPlus", ["KP_Add"];
    NumpadDivide = 0x070054, "NumpadDivide", ["KP_Divide"];
    NumpadEnter = 0x070058, "NumpadEnter", ["KP_Enter"];
    NumpadMinus = 0x070056, "NumpadMinus", ["KP_Subtract"];
    NumLock = 0x070053, "NumLock", ["NumpadClear"];
    NumpadEqual = 0x070067, "NumpadEqual";
    // --- Keyboard page — Punctuation / symbols ---
    Minus = 0x07002D, "Minus";
    Equal = 0x07002E, "Equal";
    BracketLeft = 0x07002F, "BracketLeft";
    BracketRight = 0x070030, "BracketRight";
    Backslash = 0x070031, "Backslash";
    Semicolon = 0x070033, "Semicolon";
    Quote = 0x070034, "Quote";
    Grave = 0x070035, "Grave";
    Comma = 0x070036, "Comma";
    Slash = 0x070038, "Slash";
    Period = 0x070037, "Period";
    IsoExtra = 0x070064, "IsoExtra", ["NonUSBackslash"];
    // --- Consumer page (0x0C) — Media controls ---
    PlayPause = 0x0C00CD, "PlayPause", ["Play"];
    VolumeUp = 0x0C00E9, "VolumeUp", ["VolUp"];
    VolumeDown = 0x0C00EA, "VolumeDown", ["VolDown"];
    Mute = 0x0C00E2, "Mute", ["VolMute"];
    NextTrack = 0x0C00B5, "NextTrack", ["ScanNext"];
    PreviousTrack = 0x0C00B6, "PreviousTrack", ["ScanPrev"];
    Stop = 0x0C00B7, "Stop", ["MediaStop"];
    // --- Consumer page — Display controls ---
    BrightnessUp = 0x0C006F, "BrightnessUp";
    BrightnessDown = 0x0C0070, "BrightnessDown";
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
        // Control/function cluster
        assert_eq!(parse("PrintScreen"), HidUsage::PrintScreen);
        assert_eq!(parse("ScrollLock"), HidUsage::ScrollLock);
        assert_eq!(parse("Pause"), HidUsage::Pause);
        assert_eq!(parse("Insert"), HidUsage::Insert);
        assert_eq!(parse("Menu"), HidUsage::Menu);
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
        assert_eq!(parse("NumLock"), HidUsage::NumLock);
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
        // Control/function aliases
        assert_eq!(parse("PrtSc"), HidUsage::PrintScreen);
        assert_eq!(parse("SysRq"), HidUsage::PrintScreen);
        assert_eq!(parse("Scroll"), HidUsage::ScrollLock);
        assert_eq!(parse("Break"), HidUsage::Pause);
        assert_eq!(parse("Ins"), HidUsage::Insert);
        assert_eq!(parse("ContextMenu"), HidUsage::Menu);
        assert_eq!(parse("Application"), HidUsage::Menu);
        // Numpad legacy alias
        assert_eq!(parse("NumpadClear"), HidUsage::NumLock);
        // Punctuation aliases
        assert_eq!(parse("NonUSBackslash"), HidUsage::IsoExtra);
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
        // Error message contains the input string.
        let err = HidUsage::try_from("BadKey").unwrap_err();
        assert!(err.to_string().contains("BadKey"));
    }

    #[test]
    fn unknown_key_error_escapes_control_chars() {
        // A config key name may embed newlines or ANSI escapes.
        // The error message is echoed into daemon logs and CLI output, so
        // the token must be escaped and must not smuggle control chars.
        let err =
            HidUsage::try_from("A\n\u{1b}[31mnope\u{1b}[0m").unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.chars().any(|c| c.is_ascii_control()),
            "control character leaked into error message: {msg:?}"
        );
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
            let yaml = serde_saphyr::to_string(&usage).unwrap_or_else(|e| {
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
                serde_saphyr::from_str(&yaml).unwrap_or_else(|e| {
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
        assert_eq!(HidUsage::all().len(), 115);
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
        assert_eq!(HidUsage::BracketRight.id(), 0x30);
        assert_eq!(HidUsage::Backslash.id(), 0x31);
        assert_eq!(HidUsage::Period.id(), 0x37);
        assert_eq!(HidUsage::Slash.id(), 0x38);

        // Control/function cluster
        assert_eq!(HidUsage::PrintScreen.id(), 0x46);
        assert_eq!(HidUsage::ScrollLock.id(), 0x47);
        assert_eq!(HidUsage::Pause.id(), 0x48);
        assert_eq!(HidUsage::Insert.id(), 0x49);
        assert_eq!(HidUsage::NumLock.id(), 0x53);
        assert_eq!(HidUsage::Menu.id(), 0x65);

        // Consumer page
        assert_eq!(HidUsage::PlayPause.id(), 0xCD);
        assert_eq!(HidUsage::VolumeUp.id(), 0xE9);
        assert_eq!(HidUsage::Mute.id(), 0xE2);
        assert_eq!(HidUsage::BrightnessUp.id(), 0x6F);
    }
}
