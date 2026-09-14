// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The structured key-event type shared by the e2e harness.
//!
//! A `[LogEvent]` is one key press or release identified by its canonical
//! key name (e.g. "CapsLock", "LeftControl", "A").  The harness builds the
//! expected event stream from the config and translates it into the byte
//! sequence the reader app records (see `char_translate`).

/// A single keyboard event in the expected output stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEvent {
    /// `true` for key-down, `false` for key-up.
    pub down: bool,
    /// The canonical key name (e.g. "CapsLock").
    pub key: String,
}

/// Build the string representation for a single key event, given its
/// common-key name (e.g. "LeftControl") and direction.
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
    fn event_str_formats_correctly() {
        let e = event_str("Escape", true);
        assert_eq!(e.key, "Escape");
        assert!(e.down);

        let e = event_str("Escape", false);
        assert_eq!(e.key, "Escape");
        assert!(!e.down);
    }
}
