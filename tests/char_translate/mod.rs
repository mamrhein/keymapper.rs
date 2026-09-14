// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Translates an expected key-event stream into the byte sequence that a
//! raw-mode terminal application records on stdin.
//!
//! The e2e harness compares the bytes the reader app actually receives
//! against this translation of the daemon's expected output events, so the
//! model must match what a real terminal delivers: US keyboard layout, bytes
//! emitted on key-down only, and the platform-specific treatment of the
//! Super/Cmd modifier (ignored by the Linux console and the Windows console,
//! intercepted by macOS Terminal.app as an application shortcut).
//!
//! The supported key set is deliberately conservative: letters, digits,
//! Tab, Space, Return, Escape, and Backspace.  Any other key (or a
//! non-letter Ctrl combination) panics with a clear message, because it has
//! no stable single-byte representation in character space and silently
//! comparing wrong bytes would be worse than a loud failure.

use crate::event_log::LogEvent;

/// The platform whose terminal semantics the translation follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// The Linux virtual console (VT) with a US keymap.
    Linux,
    /// macOS Terminal.app on a pty.
    Macos,
    /// The Windows console (conhost) on a pty.
    Windows,
}

/// The platform the harness is running on, selected at compile time.
pub fn current_platform() -> Platform {
    if cfg!(target_os = "linux") {
        Platform::Linux
    } else if cfg!(target_os = "macos") {
        Platform::Macos
    } else {
        Platform::Windows
    }
}

/// The modifier class of a canonical key name, if it is a modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModifierClass {
    Shift,
    Ctrl,
    /// Alt (Linux) / Option (macOS): tracked but has no effect on the
    /// supported key set; the fixtures never emit Alt combinations.
    Alt,
    /// Super (Linux/Windows) / Cmd (macOS): handled per platform.
    Cmd,
    /// CapsLock: an LED key that produces no bytes; the fixtures always map
    /// it, so it never reaches the reader in a state-affecting way.
    CapsLock,
}

fn modifier_class(key: &str) -> Option<ModifierClass> {
    match key {
        "LeftShift" | "RightShift" => Some(ModifierClass::Shift),
        "LeftControl" | "RightControl" => Some(ModifierClass::Ctrl),
        "LeftAlt" | "RightAlt" => Some(ModifierClass::Alt),
        "LeftCommand" | "RightCommand" => Some(ModifierClass::Cmd),
        "CapsLock" => Some(ModifierClass::CapsLock),
        _ => None,
    }
}

/// Translate an expected event stream into the bytes a raw-mode terminal
/// application would record.
///
/// The stream is processed by a small state machine that tracks the held
/// output modifiers.  A modifier down/up never produces bytes; a base-key
/// down produces its byte (terminals emit on key-down), shaped by the held
/// Shift and Ctrl state.  A modifier that is downed without a matching up
/// (the daemon holds output modifiers until the physical trigger releases)
/// simply stays held for the rest of the stream.
pub fn events_to_bytes(events: &[LogEvent], platform: Platform) -> Vec<u8> {
    let mut out = Vec::new();
    let mut shift = false;
    let mut ctrl = false;
    let mut cmd = false;

    for event in events {
        // Modifier keys never produce bytes: update the held state and skip
        // to the next event.  Alt and CapsLock have no effect on the
        // supported key set.
        if let Some(class) = modifier_class(&event.key) {
            match class {
                ModifierClass::Shift => shift = event.down,
                ModifierClass::Ctrl => ctrl = event.down,
                ModifierClass::Cmd => cmd = event.down,
                ModifierClass::Alt | ModifierClass::CapsLock => {}
            }
            continue;
        }

        if !event.down {
            // Bytes are emitted on key-down only; key-ups are silent.
            continue;
        }

        if platform == Platform::Macos && cmd {
            // Terminal.app consumes Cmd combinations as application
            // shortcuts (e.g. Cmd+A is Select All), so the pty receives no
            // byte at all.
            continue;
        }

        out.push(base_key_byte(&event.key, shift, ctrl));
    }

    out
}

/// The byte a base-key down produces under the given modifier state.
///
/// Panics for keys that have no stable single-byte representation in
/// character space; the e2e key set must stay within letters, digits, Tab,
/// Space, Return, Escape, and Backspace.
fn base_key_byte(key: &str, shift: bool, ctrl: bool) -> u8 {
    // Special keys: their byte is independent of Shift/Ctrl on all three
    // platforms (the terminal does not distinguish e.g. Ctrl+Return).
    match key {
        "Tab" => return 0x09,
        "Space" => return 0x20,
        "Return" => return 0x0d,
        "Escape" => return 0x1b,
        "Backspace" => return 0x7f,
        _ => {}
    }

    let c = key
        .chars()
        .next()
        .filter(|_| key.len() == 1)
        .unwrap_or_else(|| unsupported_key(key));

    if ctrl {
        // Ctrl+letter is the classic control code (Ctrl+A = 0x01, ...);
        // Ctrl+anything-else has no stable representation here.
        let lower = c.to_ascii_lowercase();
        if lower.is_ascii_alphabetic() {
            return lower as u8 & 0x1f;
        }
        unsupported_key(key)
    }

    if shift {
        match c {
            'A'..='Z' => return c as u8,
            // US layout: 1→! ... 9→( 0→), so '1'..'9' use indices 0..8 and
            // '0' uses index 9.
            '1'..='9' => return SHIFTED_DIGITS[(c as u8 - b'1') as usize],
            '0' => return SHIFTED_DIGITS[9],
            _ => unsupported_key(key),
        }
    }

    let lower = c.to_ascii_lowercase();
    if matches!(lower, 'a'..='z' | '0'..='9') {
        return lower as u8;
    }
    unsupported_key(key)
}

/// US layout shifted digits: 1→! 2→@ 3→# 4→$ 5→% 6→^ 7→& 8→* 9→( 0→)
const SHIFTED_DIGITS: [u8; 10] = *b"!@#$%^&*()";

/// Diverge with a clear message for keys outside the character-space key set.
fn unsupported_key(key: &str) -> ! {
    panic!(
        "key '{key}' is not representable in character space; the e2e key \
         set must stay within letters, digits, Tab, Space, Return, Escape, \
         and Backspace"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::event_str;

    fn tap(key: &str) -> Vec<LogEvent> {
        vec![event_str(key, true), event_str(key, false)]
    }

    fn chord(mod_key: &str, base: &str) -> Vec<LogEvent> {
        vec![
            event_str(mod_key, true),
            event_str(base, true),
            event_str(base, false),
            event_str(mod_key, false),
        ]
    }

    #[test]
    fn plain_letter_is_lowercase_on_all_platforms() {
        for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
            assert_eq!(events_to_bytes(&tap("A"), platform), b"a");
        }
    }

    #[test]
    fn shift_uppercases_letters() {
        assert_eq!(
            events_to_bytes(&chord("LeftShift", "A"), Platform::Linux),
            b"A"
        );
    }

    #[test]
    fn ctrl_produces_control_codes() {
        // Ctrl+X = 0x18, Ctrl+A = 0x01.
        assert_eq!(
            events_to_bytes(&chord("LeftControl", "X"), Platform::Linux),
            b"\x18"
        );
        assert_eq!(
            events_to_bytes(&chord("RightControl", "A"), Platform::Macos),
            b"\x01"
        );
    }

    #[test]
    fn special_keys_map_to_their_bytes() {
        assert_eq!(events_to_bytes(&tap("Tab"), Platform::Linux), b"\t");
        assert_eq!(events_to_bytes(&tap("Return"), Platform::Linux), b"\r");
        assert_eq!(events_to_bytes(&tap("Escape"), Platform::Linux), b"\x1b");
        assert_eq!(
            events_to_bytes(&tap("Backspace"), Platform::Linux),
            b"\x7f"
        );
        assert_eq!(events_to_bytes(&tap("Space"), Platform::Linux), b" ");
    }

    #[test]
    fn bare_modifiers_produce_no_bytes() {
        let events = vec![
            event_str("LeftShift", true),
            event_str("LeftShift", false),
            event_str("RightControl", true),
            event_str("RightControl", false),
        ];
        assert_eq!(events_to_bytes(&events, Platform::Linux), b"");
    }

    #[test]
    fn cmd_letter_is_intercepted_on_macos_only() {
        let events = chord("LeftCommand", "A");
        assert_eq!(events_to_bytes(&events, Platform::Macos), b"");
        assert_eq!(events_to_bytes(&events, Platform::Linux), b"a");
        assert_eq!(events_to_bytes(&events, Platform::Windows), b"a");
    }

    #[test]
    fn held_modifier_without_release_stays_held() {
        // Models the chord test: the CapsLock→LeftControl rule emits a held
        // LeftControl (no up yet), then the probe key X is pressed while it
        // is held, producing Ctrl+X.
        let events = vec![
            event_str("LeftControl", true),
            event_str("X", true),
            event_str("X", false),
        ];
        assert_eq!(events_to_bytes(&events, Platform::Linux), b"\x18");
    }

    #[test]
    fn full_chord_sequence_translates_to_one_byte() {
        // The complete chord: held modifier down, probe tap, modifier up.
        let events = vec![
            event_str("LeftControl", true),
            event_str("X", true),
            event_str("X", false),
            event_str("LeftControl", false),
        ];
        for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
            assert_eq!(events_to_bytes(&events, platform), b"\x18");
        }
    }

    #[test]
    fn digits_and_shifted_digits() {
        assert_eq!(events_to_bytes(&tap("1"), Platform::Linux), b"1");
        assert_eq!(
            events_to_bytes(&chord("LeftShift", "1"), Platform::Linux),
            b"!"
        );
        assert_eq!(events_to_bytes(&tap("0"), Platform::Windows), b"0");
    }

    #[test]
    fn key_ups_produce_no_bytes() {
        let events = vec![event_str("A", false), event_str("B", false)];
        assert_eq!(events_to_bytes(&events, Platform::Linux), b"");
    }

    #[test]
    fn mixed_sequence_accumulates_in_order() {
        // A tap, a Shift chord, and an Escape tap: "aA\x1b".
        let events =
            [tap("A"), chord("LeftShift", "B"), tap("Escape")].concat();
        assert_eq!(events_to_bytes(&events, Platform::Linux), b"aB\x1b");
    }

    #[test]
    #[should_panic(expected = "not representable in character space")]
    fn unsupported_key_panics() {
        let _ = events_to_bytes(&tap("Delete"), Platform::Linux);
    }

    #[test]
    #[should_panic(expected = "not representable in character space")]
    fn ctrl_on_non_letter_panics() {
        let _ = events_to_bytes(&chord("LeftControl", "1"), Platform::Linux);
    }

    #[test]
    fn current_platform_matches_compile_target() {
        let expected = if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "macos") {
            Platform::Macos
        } else {
            Platform::Windows
        };
        assert_eq!(current_platform(), expected);
    }
}
