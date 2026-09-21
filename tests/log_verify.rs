// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Log-based verification for the e2e harness.
//!
//! This replaces the reader's byte comparison.  Instead of comparing the
//! bytes a raw-mode application records, the harness reads the daemon's own
//! debug log and checks three things for each phase:
//!
//! 1. **emit sequence equality** — the parsed `emit` lines equal, in order and
//!    exactly, the expected sequence derived from the config.
//! 2. **passthrough presence** — every key that must pass through (trigger
//!    modifiers, the chord probe key, the fixed passthrough keys) appears in a
//!    `recv` line and in a `pass` line for its down.  A key that had been
//!    remapped instead would have no `pass` line, so this is also the "not
//!    remapped" check; the exact emit sequence (check 1) pins down what was
//!    actually emitted.
//! 3. **no error lines** — zero `ERROR`-level lines in the window (catches
//!    failed emits, e.g. the Linux `Emit error`).
//!
//! The parser understands the unified debug-log grammar shared by all three
//! platforms:
//!
//! ```text
//! recv <native> <down|up|repeat> -> <usage>
//! pass <native> <down|up|repeat> -> <usage>
//! swal <native> <down|up|repeat> -> <usage>
//! emit <native_key>
//! ```
//!
//! `<native>` is platform-specific free text (a device path and keycode on
//! Linux, a keycode on macOS, a vk code on Windows) that the parser ignores.
//! A full log line is `{timestamp} {LEVEL} {target}: {message}` (no
//! timestamp on Linux); the parser extracts the level and the message.  The
//! `map` line the engine emits is not part of the verification and is
//! ignored.

use keymapper::common::hid_usage::HidUsage;

/// The direction of a key event in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A key-down.
    Down,
    /// A key-up.
    Up,
    /// An auto-repeat (treated as a down by the engine).
    Repeat,
}

/// Which kind of `recv`/`pass`/`swal` line a parsed event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The daemon received the key.
    Recv,
    /// The daemon forwarded the key to the application.
    Pass,
    /// The daemon swallowed the key.
    Swal,
}

/// A parsed `recv`/`pass`/`swal` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    /// Which line it was.
    pub kind: Kind,
    /// The key's HID usage, parsed from the trailing `-> <usage>`.
    pub usage: HidUsage,
    /// The event direction.
    pub action: Action,
}

/// The expected model for one phase, built from the config (see
/// `build_test_sequences` in the e2e harness).
#[derive(Debug, Clone)]
pub struct ExpectedPhase {
    /// The expected `emit` lines, in order (rendered `NativeKey`s).
    pub emits: Vec<String>,
    /// The keys that must pass through: they appear in a `recv` line and a
    /// `pass` line for their down, and are never remapped.  Trigger
    /// modifiers, the chord probe key, and the fixed passthrough keys.
    pub passthrough_keys: Vec<HidUsage>,
    /// Every key the phase injects (informational, for failure messages).
    pub injected_keys: Vec<HidUsage>,
}

/// The parsed log window for one phase.
#[derive(Debug, Clone, Default)]
pub struct ParsedPhase {
    /// The `emit` lines, in order.
    pub emits: Vec<String>,
    /// All parsed `recv`/`pass`/`swal` events, in order.
    pub key_events: Vec<KeyEvent>,
    /// The raw `ERROR`-level lines.
    pub error_lines: Vec<String>,
    /// The raw log lines in the window, kept for failure dumps.
    pub window: Vec<String>,
}

/// Parse a raw log line into its level and message.
///
/// Returns `None` for lines that do not carry a `{target}: {message}` body.
/// The level is reported only as "is this an error line", which is all the
/// verification needs.
fn split_line(line: &str) -> Option<(bool, &str)> {
    // The first `": "` is always the target/message separator: the timestamp
    // (file platforms) and the module-path target contain colons but never a
    // colon followed by a space.
    let sep = line.find(": ")?;
    let head = &line[..sep];
    let message = &line[sep + 2..];
    let is_error = head.split_whitespace().any(|token| token == "ERROR");
    Some((is_error, message))
}

/// The parsed body of a non-error log line.
enum Message {
    /// A `recv`/`pass`/`swal` key event.
    Key(KeyEvent),
    /// An `emit` line, with the rendered output key.
    Emit(String),
}

/// Parse a log message body against the unified grammar.
fn parse_message(message: &str) -> Option<Message> {
    if let Some(rest) = message.strip_prefix("recv ") {
        return parse_key_event(Kind::Recv, rest);
    }
    if let Some(rest) = message.strip_prefix("pass ") {
        return parse_key_event(Kind::Pass, rest);
    }
    if let Some(rest) = message.strip_prefix("swal ") {
        return parse_key_event(Kind::Swal, rest);
    }
    if let Some(rest) = message.strip_prefix("emit ") {
        // The rendered key has no spaces (modifiers are joined with `+`), so
        // the whole remainder is the output key.
        return Some(Message::Emit(rest.to_string()));
    }
    // `map` lines and everything else are not part of the verification.
    None
}

/// Parse a `<native> <action> -> <usage>` body into a [`KeyEvent`].
fn parse_key_event(kind: Kind, rest: &str) -> Option<Message> {
    // The usage is the last field; split it off at the final ` -> `.  The
    // native field may itself contain spaces (Linux: path and keycode), so it
    // is everything before the action and is ignored.
    let arrow = rest.rfind(" -> ")?;
    let left = &rest[..arrow];
    let usage_str = &rest[arrow + " -> ".len()..];

    // The action is the last space-separated token of the left part.
    let action_str = left.rsplit(' ').next()?;
    let action = match action_str {
        "down" => Action::Down,
        "up" => Action::Up,
        "repeat" => Action::Repeat,
        _ => return None,
    };

    let usage = HidUsage::try_from(usage_str).ok()?;
    Some(Message::Key(KeyEvent {
        kind,
        usage,
        action,
    }))
}

/// Parse a window of raw log lines into a [`ParsedPhase`].
pub fn parse_lines(lines: &[&str]) -> ParsedPhase {
    let mut parsed = ParsedPhase::default();
    for line in lines {
        parsed.window.push(line.to_string());
        let Some((is_error, message)) = split_line(line) else {
            continue;
        };
        if is_error {
            parsed.error_lines.push(line.to_string());
            continue;
        }
        match parse_message(message) {
            Some(Message::Key(event)) => parsed.key_events.push(event),
            Some(Message::Emit(key)) => parsed.emits.push(key),
            None => {}
        }
    }
    parsed
}

/// Verify a parsed phase against the expected model.
///
/// Returns `Ok(())` when all three checks pass, or `Err` with a readable
/// failure message that includes the phase's log window.
pub fn verify_phase(
    expected: &ExpectedPhase,
    parsed: &ParsedPhase,
) -> Result<(), String> {
    let mut problems: Vec<String> = Vec::new();

    // 1. Emit sequence equality (ordered, exact).
    if parsed.emits != expected.emits {
        problems.push(format!(
            "emit sequence mismatch:\n  expected: {}\n  actual:   {}",
            render_list(&expected.emits),
            render_list(&parsed.emits)
        ));
    }

    // 2. Passthrough presence: each key must appear in a recv line and in a
    //    pass line for its down.
    for key in &expected.passthrough_keys {
        let name = key.as_str();
        let has_recv = parsed
            .key_events
            .iter()
            .any(|e| e.kind == Kind::Recv && e.usage == *key);
        let has_pass_down = parsed.key_events.iter().any(|e| {
            e.kind == Kind::Pass && e.usage == *key && e.action == Action::Down
        });
        if !has_recv {
            problems.push(format!("passthrough key {name} has no recv line"));
        }
        if !has_pass_down {
            problems.push(format!(
                "passthrough key {name} has no pass (down) line"
            ));
        }
    }

    // 3. No error lines.
    if !parsed.error_lines.is_empty() {
        problems.push(format!(
            "{} ERROR line(s) in the log window:\n{}",
            parsed.error_lines.len(),
            parsed.error_lines.join("\n")
        ));
    }

    if problems.is_empty() {
        return Ok(());
    }

    Err(format!(
        "{}\n\nlog window:\n{}",
        problems.join("\n\n"),
        dump_window(&parsed.window)
    ))
}

/// Parse a window of raw log lines and verify it against the expected model
/// in one step.  Convenience wrapper for the harness.
pub fn verify_window(
    expected: &ExpectedPhase,
    lines: &[&str],
) -> Result<(), String> {
    let parsed = parse_lines(lines);
    verify_phase(expected, &parsed)
}

/// Render a list of emit keys as a single readable line.
fn render_list(keys: &[String]) -> String {
    if keys.is_empty() {
        return "(none)".to_string();
    }
    keys.join(", ")
}

/// Render the log window as an indented block for a failure message.
fn dump_window(window: &[String]) -> String {
    if window.is_empty() {
        return "(empty)".to_string();
    }
    window
        .iter()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parser: line splitting and level detection -------------------------

    #[test]
    fn splits_linux_line_without_timestamp() {
        let (is_error, message) = split_line(
            "DEBUG keymapper::x: recv /dev/input/event3 30 down -> A",
        )
        .unwrap();
        assert!(!is_error);
        assert_eq!(message, "recv /dev/input/event3 30 down -> A");
    }

    #[test]
    fn splits_file_platform_line_with_timestamp() {
        let (is_error, message) = split_line(
            "2026-09-21 14:23:24.123+02:00 DEBUG keymapper::x: emit B",
        )
        .unwrap();
        assert!(!is_error);
        assert_eq!(message, "emit B");
    }

    #[test]
    fn detects_error_level() {
        let (is_error, _) =
            split_line("ERROR keymapper::x: Emit error: device busy").unwrap();
        assert!(is_error);
    }

    #[test]
    fn rejects_line_without_target_separator() {
        assert!(split_line("no separator here").is_none());
    }

    // --- parser: key events -------------------------------------------------

    #[test]
    fn parses_linux_recv_with_path_and_keycode() {
        let parsed = parse_lines(&[
            "DEBUG keymapper::x: recv /dev/input/event3 30 down -> A"
        ]);
        assert_eq!(
            parsed.key_events,
            vec![KeyEvent {
                kind: Kind::Recv,
                usage: HidUsage::A,
                action: Action::Down,
            }]
        );
    }

    #[test]
    fn parses_macos_recv_with_keycode() {
        let parsed = parse_lines(&["2026-09-21 14:23:24.123+02:00 DEBUG \
                                    keymapper::x: recv keycode=8 down -> A"]);
        assert_eq!(
            parsed.key_events,
            vec![KeyEvent {
                kind: Kind::Recv,
                usage: HidUsage::A,
                action: Action::Down,
            }]
        );
    }

    #[test]
    fn parses_windows_recv_with_vk() {
        let parsed = parse_lines(&["2026-09-21 14:23:24.123+02:00 DEBUG \
                                    keymapper::x: recv vk=65 down -> A"]);
        assert_eq!(
            parsed.key_events,
            vec![KeyEvent {
                kind: Kind::Recv,
                usage: HidUsage::A,
                action: Action::Down,
            }]
        );
    }

    #[test]
    fn parses_pass_and_swal_and_up_direction() {
        let parsed = parse_lines(&[
            "DEBUG keymapper::x: pass /dev/input/event3 30 down -> D",
            "TRACE keymapper::x: pass /dev/input/event3 30 up -> D",
            "TRACE keymapper::x: swal /dev/input/event3 1 down -> A",
        ]);
        assert_eq!(
            parsed.key_events,
            vec![
                KeyEvent {
                    kind: Kind::Pass,
                    usage: HidUsage::D,
                    action: Action::Down,
                },
                KeyEvent {
                    kind: Kind::Pass,
                    usage: HidUsage::D,
                    action: Action::Up,
                },
                KeyEvent {
                    kind: Kind::Swal,
                    usage: HidUsage::A,
                    action: Action::Down,
                },
            ]
        );
    }

    #[test]
    fn parses_emit_with_modifier_chord() {
        let parsed = parse_lines(&[
            "DEBUG keymapper::x: emit LeftCommand+A",
            "DEBUG keymapper::x: emit B",
        ]);
        assert_eq!(parsed.emits, vec!["LeftCommand+A", "B"]);
    }

    #[test]
    fn ignores_map_and_unknown_lines() {
        let parsed = parse_lines(&[
            "DEBUG keymapper::daemon::engine: map A -> [B]",
            "INFO keymapper::x: Configuration hot-swapped successfully!",
            "garbage line without a target",
        ]);
        assert!(parsed.emits.is_empty());
        assert!(parsed.key_events.is_empty());
        assert!(parsed.error_lines.is_empty());
    }

    #[test]
    fn collects_error_lines() {
        let parsed = parse_lines(&[
            "DEBUG keymapper::x: emit B",
            "ERROR keymapper::x: Emit error: device busy",
        ]);
        assert_eq!(parsed.emits, vec!["B"]);
        assert_eq!(parsed.error_lines.len(), 1);
        assert!(parsed.error_lines[0].contains("Emit error"));
    }

    // --- verifier -----------------------------------------------------------

    fn expected(emits: &[&str], passthrough: &[HidUsage]) -> ExpectedPhase {
        ExpectedPhase {
            emits: emits.iter().map(|s| s.to_string()).collect(),
            passthrough_keys: passthrough.to_vec(),
            injected_keys: Vec::new(),
        }
    }

    #[test]
    fn verify_passes_on_matching_phase() {
        let exp = expected(&["B"], &[HidUsage::D]);
        let lines = &[
            "DEBUG keymapper::x: recv /dev/input/event3 4 down -> D",
            "DEBUG keymapper::x: pass /dev/input/event3 4 down -> D",
            "DEBUG keymapper::x: recv /dev/input/event3 30 down -> A",
            "DEBUG keymapper::x: emit B",
        ];
        assert!(verify_window(&exp, lines).is_ok());
    }

    #[test]
    fn verify_fails_on_emit_mismatch() {
        let exp = expected(&["B", "C"], &[HidUsage::D]);
        let lines = &[
            "DEBUG keymapper::x: recv /dev/input/event3 4 down -> D",
            "DEBUG keymapper::x: pass /dev/input/event3 4 down -> D",
            "DEBUG keymapper::x: emit B",
        ];
        let err = verify_window(&exp, lines).unwrap_err();
        assert!(err.contains("emit sequence mismatch"));
        assert!(err.contains("expected: B, C"));
        assert!(err.contains("actual:   B"));
        // The failure dumps the log window.
        assert!(err.contains("log window:"));
    }

    #[test]
    fn verify_fails_on_missing_passthrough_recv() {
        let exp = expected(&[], &[HidUsage::D]);
        // D is passed through but never received (should not happen, but the
        // check must catch it).
        let lines =
            &["DEBUG keymapper::x: pass /dev/input/event3 4 down -> D"];
        let err = verify_window(&exp, lines).unwrap_err();
        assert!(err.contains("passthrough key D has no recv line"));
    }

    #[test]
    fn verify_fails_on_missing_passthrough_pass_down() {
        let exp = expected(&[], &[HidUsage::D]);
        // D is received but swallowed instead of passed through: the remap
        // leaked, so there is no pass (down) line.
        let lines = &[
            "DEBUG keymapper::x: recv /dev/input/event3 4 down -> D",
            "TRACE keymapper::x: swal /dev/input/event3 4 down -> D",
        ];
        let err = verify_window(&exp, lines).unwrap_err();
        assert!(err.contains("passthrough key D has no pass (down) line"));
    }

    #[test]
    fn verify_fails_on_error_line() {
        let exp = expected(&["B"], &[]);
        let lines = &[
            "DEBUG keymapper::x: emit B",
            "ERROR keymapper::x: Emit error: device busy",
        ];
        let err = verify_window(&exp, lines).unwrap_err();
        assert!(err.contains("ERROR line(s)"));
        assert!(err.contains("Emit error: device busy"));
    }

    #[test]
    fn verify_reports_all_problems_at_once() {
        let exp = expected(&["B"], &[HidUsage::D]);
        // Wrong emit, missing passthrough, and an error line all at once.
        let lines = &[
            "DEBUG keymapper::x: emit C",
            "ERROR keymapper::x: Emit error: device busy",
        ];
        let err = verify_window(&exp, lines).unwrap_err();
        assert!(err.contains("emit sequence mismatch"));
        assert!(err.contains("passthrough key D has no recv line"));
        assert!(err.contains("ERROR line(s)"));
    }
}
