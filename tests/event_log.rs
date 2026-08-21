// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Parses the monitor's event log file and provides assertion helpers.
//!
//! The monitor writes one line per event in the format `down <Key>` or
//! `up <Key>`, where `<Key>` is a canonical key name (e.g. "CapsLock",
//! "LeftControl", "A"). This module reads those lines into structured
//! events and compares them against expected sequences.

use std::{fs, path::Path};

/// A single keyboard event parsed from the monitor's output log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEvent {
    /// `true` for key-down, `false` for key-up.
    pub down: bool,
    /// The canonical key name as written by the monitor (e.g. "CapsLock").
    pub key: String,
}

/// Parse an event log file into a list of `[LogEvent]`.
///
/// Lines that are empty or cannot be parsed are silently skipped.
pub fn parse(path: &Path) -> std::io::Result<Vec<LogEvent>> {
    let content = fs::read_to_string(path)?;
    let mut events = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Some((direction, key)) = line.split_once(' ') else {
            continue;
        };

        let down = match direction {
            "down" => true,
            "up" => false,
            _ => continue,
        };

        events.push(LogEvent {
            down,
            key: key.to_string(),
        });
    }

    Ok(events)
}

/// Assert that *actual* events match *expected* exactly.
///
/// Produces a descriptive diff on mismatch showing all positions and values
/// that diverge, plus length differences if applicable.
pub fn assert_events_match(
    actual: &[LogEvent],
    expected: &[LogEvent],
    message: &str,
) {
    if actual.len() != expected.len() {
        let actual_str: Vec<String> =
            actual.iter().map(format_event).collect();
        let expected_str: Vec<String> =
            expected.iter().map(format_event).collect();

        panic!(
            "{}\nlength mismatch: got {} events, expected {}\nactual  = \
             [{combined_actual}]\nexpected = [{combined_expected}]",
            message,
            actual.len(),
            expected.len(),
            combined_actual = actual_str.join(", "),
            combined_expected = expected_str.join(", "),
        );
    }

    let mut mismatches: Vec<(usize, &LogEvent, &LogEvent)> = Vec::new();
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        if a != e {
            mismatches.push((i, a, e));
        }
    }

    if mismatches.is_empty() {
        return;
    }

    let mut detail_lines: Vec<String> = mismatches
        .iter()
        .map(|(i, a, e)| {
            format!(
                "event mismatch at position {i}\nactual[{}]   = \
                 {}\nexpected[{}] = {}",
                i,
                format_event(a),
                i,
                format_event(e),
            )
        })
        .collect();

    // Reverse so mismatches print in ascending index order when joined.
    detail_lines.reverse();

    panic!("{}\n{}", message, detail_lines.join("\n"));
}

/// Format a single `[LogEvent]` as a human-readable string for diffs.
fn format_event(event: &LogEvent) -> String {
    let dir = if event.down { "down" } else { "up" };
    format!("{dir} {}", event.key)
}

/// Build the string representation for a single key event, given its
/// common-key name (e.g. "LeftControl") and the down direction.
pub fn event_str(key: &str, down: bool) -> LogEvent {
    LogEvent {
        down,
        key: key.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_file() {
        // `NamedTempFile` creates a unique file that is removed on drop,
        // so concurrent test runs cannot collide or delete each other's
        // files.
        let file = tempfile::NamedTempFile::new().unwrap();

        let events = parse(file.path()).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn parse_basic_events() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), "down CapsLock\nup CapsLock\ndown A\nup A\n")
            .unwrap();

        let events = parse(file.path()).unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(
            events,
            vec![
                event_str("CapsLock", true),
                event_str("CapsLock", false),
                event_str("A", true),
                event_str("A", false),
            ]
        );
    }

    #[test]
    fn parse_skips_empty_and_malformed_lines() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            file.path(),
            "down LeftControl\n\nbadline\nup LeftControl\n   \n",
        )
        .unwrap();

        let events = parse(file.path()).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events,
            vec![
                event_str("LeftControl", true),
                event_str("LeftControl", false),
            ]
        );
    }

    #[test]
    fn assert_events_match_passes_on_identical() {
        let events = vec![event_str("A", true), event_str("A", false)];
        // Should not panic.
        assert_events_match(&events, &events, "identical sequences");
    }

    #[test]
    #[should_panic(expected = "length mismatch")]
    fn assert_events_match_fails_on_length_diff() {
        let actual = vec![event_str("A", true)];
        let expected = vec![event_str("A", true), event_str("A", false)];
        assert_events_match(&actual, &expected, "length test");
    }

    #[test]
    #[should_panic(expected = "event mismatch at position 0")]
    fn assert_events_match_fails_on_value_diff() {
        let actual = vec![event_str("B", true)];
        let expected = vec![event_str("A", true)];
        assert_events_match(&actual, &expected, "value test");
    }

    #[test]
    #[should_panic(expected = "event mismatch at position 0")]
    fn assert_events_match_reports_all_mismatches() {
        let actual = vec![
            event_str("B", true),
            event_str("X", false),
            event_str("C", true),
        ];
        let expected = vec![
            event_str("A", true),
            event_str("Y", false),
            event_str("Z", true),
        ];
        assert_events_match(&actual, &expected, "multi-mismatch test");
    }

    #[test]
    fn event_str_formats_correctly() {
        let e = event_str("Escape", true);
        assert_eq!(e.key, "Escape");
        assert!(e.down);

        let e = event_str("Escape", false);
        assert!(!e.down);
    }
}
