// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Shared evdev capture and scan-code decode.
//!
//! Both the daemon's grabbed capture path (`mapping::device`) and the
//! read-only observe mode consumed by `keymapper keys probe`
//! ([`LinuxBackend::observe`](super::backend::LinuxBackend)) feed their
//! raw evdev events through [`KeyScanner`], so the `MSC_SCAN` buffering
//! and the HID decode step have exactly one implementation and the two
//! paths cannot drift.

use evdev::{EventType, InputEvent, MiscCode};

use crate::{
    common::hid_usage::HidUsage,
    platform::{
        backend::CapturedKey, linux::hid_translate::keycode_to_hid_usage,
    },
};

/// Turns raw evdev events into [`CapturedKey`]s.
///
/// The kernel emits an `MSC_SCAN` event with the raw HID usage
/// `(page << 16) | id` before the `EV_KEY` event of the same key press;
/// key-ups, auto-repeats, and devices without `MSC_SCAN` carry no scan
/// code, so those fall back to the `EV_KEY` reverse lookup.  The scanner
/// owns that buffering across events.
pub(super) struct KeyScanner {
    /// Last received `MSC_SCAN` value, consumed by the next `EV_KEY`
    /// event.
    pending_scan: Option<u32>,
}

impl KeyScanner {
    pub(super) fn new() -> Self {
        Self { pending_scan: None }
    }

    /// Feed one raw event.  Returns a [`CapturedKey`] for `EV_KEY`
    /// events (press, release, or auto-repeat), `None` for everything
    /// else (including the buffered `MSC_SCAN` event itself).
    pub(super) fn on_event(
        &mut self,
        event: InputEvent,
    ) -> Option<CapturedKey> {
        if event.event_type() == EventType::MISC
            && event.code() == MiscCode::MSC_SCAN.0
        {
            self.pending_scan = Some(event.value() as u32);
            return None;
        }

        if event.event_type() != EventType::KEY {
            return None;
        }

        let native = event.code();
        // Prefer the raw HID usage from MSC_SCAN; the EV_KEY reverse
        // lookup covers key-ups, auto-repeats, and devices that do not
        // emit MSC_SCAN.
        let usage = self
            .pending_scan
            .take()
            .and_then(HidUsage::from_code)
            .or_else(|| keycode_to_hid_usage(native));

        Some(CapturedKey {
            native,
            value: event.value(),
            usage,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap_core::logfmt::Direction;

    const EV_SYN: u16 = EventType::SYNCHRONIZATION.0;
    const EV_KEY: u16 = EventType::KEY.0;
    const EV_MISC: u16 = EventType::MISC.0;
    const MSC_SCAN: u16 = MiscCode::MSC_SCAN.0;

    fn misc(value: i32) -> InputEvent {
        InputEvent::new(EV_MISC, MSC_SCAN, value)
    }

    fn key(code: u16, value: i32) -> InputEvent {
        InputEvent::new(EV_KEY, code, value)
    }

    /// The MSC_SCAN value the kernel reports for the 'A' key: HID
    /// generic page (0x07) usage 0x04.
    const SCAN_A: i32 = 0x0007_0004;

    #[test]
    fn scan_code_is_consumed_by_the_next_key_event() {
        let mut scanner = KeyScanner::new();
        // The MSC_SCAN event itself yields nothing; the following
        // key-down carries the decoded usage.
        assert!(scanner.on_event(misc(SCAN_A)).is_none());
        let captured = scanner.on_event(key(30, 1)).expect("key event");
        assert_eq!(captured.native, 30);
        assert_eq!(captured.value, 1);
        assert_eq!(captured.direction(), Direction::Down);
        assert_eq!(captured.usage, Some(HidUsage::A));
    }

    #[test]
    fn key_up_without_scan_code_falls_back_to_the_table() {
        let mut scanner = KeyScanner::new();
        // A key-up carries no MSC_SCAN; the reverse lookup resolves it.
        let captured = scanner.on_event(key(30, 0)).expect("key event");
        assert_eq!(captured.direction(), Direction::Up);
        assert_eq!(captured.usage, Some(HidUsage::A));
    }

    #[test]
    fn unresolvable_scan_code_falls_back_to_the_table() {
        let mut scanner = KeyScanner::new();
        // A vendor-defined scan code `HidUsage::from_code` cannot map
        // must not suppress the EV_KEY reverse lookup.
        scanner.on_event(misc(0x00FF_FFFF));
        let captured = scanner.on_event(key(30, 1)).expect("key event");
        assert_eq!(captured.usage, Some(HidUsage::A));
    }

    #[test]
    fn scan_code_does_not_leak_to_later_events() {
        let mut scanner = KeyScanner::new();
        scanner.on_event(misc(SCAN_A));
        scanner.on_event(key(30, 1)); // consumes the scan code
        // The following up and the next press resolve through the table,
        // unaffected by the already-consumed scan code.
        assert_eq!(
            scanner.on_event(key(30, 0)).unwrap().usage,
            Some(HidUsage::A)
        );
        // evdev home row: A=30, S=31.
        assert_eq!(
            scanner.on_event(key(31, 1)).unwrap().usage,
            Some(HidUsage::S)
        );
    }

    #[test]
    fn auto_repeat_is_reported_as_repeat_direction() {
        let mut scanner = KeyScanner::new();
        let captured = scanner.on_event(key(30, 2)).expect("repeat event");
        assert_eq!(captured.direction(), Direction::Repeat);
    }

    #[test]
    fn non_key_events_produce_no_captured_key() {
        let mut scanner = KeyScanner::new();
        assert!(scanner.on_event(InputEvent::new(EV_SYN, 0, 0)).is_none());
        assert!(scanner.on_event(misc(0)).is_none());
        // An LED event is also not a key event.
        assert!(
            scanner
                .on_event(InputEvent::new(EventType::LED.0, 0, 0))
                .is_none()
        );
    }

    #[test]
    fn unknown_code_captures_without_usage() {
        let mut scanner = KeyScanner::new();
        // 729 is beyond the translation table's coverage.
        let captured = scanner.on_event(key(729, 1)).expect("key event");
        assert_eq!(captured.native, 729);
        assert_eq!(captured.usage, None);
    }
}
