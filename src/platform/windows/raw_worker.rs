// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Raw input thread: device-identification buffer and consumer events.
//!
//! Consumes the [`RawInputEvent`] stream from the raw input message loop:
//!
//! - Keyboard key-downs are appended to the shared matching buffer
//!   ([`super::device_match`]) that the hook proc consults for per-rule
//!   keyboard filtering.  Key-ups carry no new device information, so they are
//!   dropped (the hook proc decides key-ups on its own lookup).
//! - Standalone Consumer Control events (e.g. media keys on a USB keypad)
//!   never reach the low-level hook, so this thread performs the mapping
//!   lookup itself and queues the resolved outputs for the main message loop.
//!   A `SendInput` issued from this thread could race a keyboard hook chain in
//!   progress and be dropped by the input system, which is why the emission
//!   goes through the same deferred queue as any other main-loop emission.

use std::sync::Arc;

use crossbeam_channel::Receiver;
use parking_lot::RwLock;

#[cfg(not(test))]
use crate::platform::windows::mapping::queue_emission;
// `capture_enabled` and `emit_key_event` are only referenced from the
// e2e-gated capture branch below, and `queue_emission` is compiled out of
// unit tests, so each is imported only where its call site exists.
#[cfg(feature = "e2e")]
use crate::platform::windows::mapping::{capture_enabled, emit_key_event};
// The emission helpers are only called on paths that are compiled out of
// unit tests (see `process_consumer_event`); the imports stay
// unconditional so the non-capture build compiles on every target.
use crate::{
    common::hid_usage::{HidUsage, PAGE_CONSUMER},
    daemon::state::Lookup,
    platform::windows::{
        device_match::{device_cache, evict, push_event},
        mapping::extract_modifier_bits,
        raw_input::RawInputEvent,
    },
};

/// Spawns the raw input thread.
///
/// The thread writes to the process-wide device-identification buffer and
/// reads the process-wide device path cache (see
/// [`super::device_match`]); it exits when the raw input channel closes
/// (process shutdown).  The hook proc needs no channel to this thread —
/// it reads the matching buffer directly.
pub(crate) fn spawn_raw_worker(
    lookup: Arc<RwLock<dyn Lookup>>,
    raw_rx: Receiver<RawInputEvent>,
) {
    std::thread::Builder::new()
        .name("keymapper-rawinput".into())
        .spawn(move || raw_worker_loop(lookup, raw_rx))
        .expect("failed to spawn raw input thread");
}

/// Main loop of the raw input thread.
///
/// Processes every raw input event until the raw input channel closes.
fn raw_worker_loop(
    lookup: Arc<RwLock<dyn Lookup>>,
    raw_rx: Receiver<RawInputEvent>,
) {
    for event in raw_rx.iter() {
        process_raw_event(&event, &lookup);
    }
}

/// Route one raw input event: buffer keyboard key-downs for device
/// matching, process standalone consumer events directly.
fn process_raw_event(event: &RawInputEvent, lookup: &Arc<RwLock<dyn Lookup>>) {
    // Buffer only key-down events; the hook proc matches on them, and
    // key-ups are decided on the hook proc's own lookup.
    if event.is_key_up {
        return;
    }

    // Standalone Consumer Control events have no VK code and no hook event
    // to match against, so process them directly.  Keyboard media keys
    // (e.g. VK_MEDIA_PLAY_PAUSE) do have a VK code and a hook event; they
    // must be buffered for matching like any other keyboard event.
    if event.vk_code.is_none()
        && let Some(usage) = event.usage
        && usage.page() == PAGE_CONSUMER
    {
        process_consumer_event(event, usage, lookup);
        return;
    }

    push_event(event.clone());
    evict();
}

/// Process a Consumer Page key-down event from a standalone Consumer
/// Control device.
///
/// These events do not pass through the low-level keyboard hook, so there
/// is no hook event to decide on: the lookup happens here.  The original
/// media action cannot be suppressed — Windows delivers Consumer Control
/// input to the shell as `WM_APPCOMMAND`, which no keyboard-level hook can
/// intercept — so a mapped media key produces both the original action and
/// the remapped output.
fn process_consumer_event(
    event: &RawInputEvent,
    usage: HidUsage,
    lookup: &Arc<RwLock<dyn Lookup>>,
) {
    let modifiers = extract_modifier_bits();
    let device_path = device_cache().get_or_resolve(event.device_handle_ptr);

    let guard = lookup.read();
    let outputs = guard
        .for_active_app(usage, modifiers, device_path.as_deref())
        .or_else(|| guard.global(usage, modifiers, device_path.as_deref()))
        .map(|v| v.to_vec());
    drop(guard);

    let Some(outputs) = outputs else {
        return;
    };

    // Capture mode (e2e only): emit the tagged output directly on this
    // non-hook thread, as with mapped keyboard events.
    #[cfg(feature = "e2e")]
    if capture_enabled() {
        for native_key in &outputs {
            emit_key_event(native_key);
        }
        return;
    }

    // Normal mode: queue for the main message loop.  A `SendInput` issued
    // directly from this thread can race a keyboard hook chain in progress
    // and be dropped by the input system.  Compiled out of unit tests so
    // they never drive a real `SendInput`.
    #[cfg(not(test))]
    {
        queue_emission(outputs);
    }

    #[cfg(test)]
    let _ = outputs;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        common::hid_usage::HidUsage, daemon::mapping_cache::NativeKey,
        platform::windows::device_match::match_usage,
    };

    /// A minimal `Lookup` stub for the consumer-event tests.
    struct MockLookup {
        global_map: Vec<(HidUsage, Vec<NativeKey>)>,
    }

    impl MockLookup {
        fn with_global(map: Vec<(HidUsage, Vec<NativeKey>)>) -> Self {
            Self { global_map: map }
        }
    }

    impl std::fmt::Debug for MockLookup {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MockLookup").finish()
        }
    }

    impl Lookup for MockLookup {
        fn for_app(
            &self,
            _app: &str,
            usage: HidUsage,
            modifiers: u8,
            keyboard_device_id: Option<&str>,
        ) -> Option<&[NativeKey]> {
            self.global(usage, modifiers, keyboard_device_id)
        }

        fn global(
            &self,
            usage: HidUsage,
            _modifiers: u8,
            _keyboard_device_id: Option<&str>,
        ) -> Option<&[NativeKey]> {
            self.global_map
                .iter()
                .find(|(u, _)| *u == usage)
                .map(|(_, outputs)| outputs.as_slice())
        }

        fn for_active_app(
            &self,
            usage: HidUsage,
            modifiers: u8,
            keyboard_device_id: Option<&str>,
        ) -> Option<&[NativeKey]> {
            self.global(usage, modifiers, keyboard_device_id)
        }
    }

    fn empty_lookup() -> Arc<RwLock<dyn Lookup>> {
        Arc::new(RwLock::new(MockLookup::with_global(vec![])))
    }

    /// Build a standalone consumer event (no VK code).
    fn consumer_event(usage: HidUsage, device: usize) -> RawInputEvent {
        RawInputEvent {
            usage: Some(usage),
            vk_code: None,
            is_key_up: false,
            device_handle_ptr: device,
        }
    }

    /// Build a keyboard key-down event (with a VK code).
    fn keyboard_event(usage: HidUsage, device: usize) -> RawInputEvent {
        RawInputEvent {
            usage: Some(usage),
            vk_code: Some(
                windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0x04),
            ),
            is_key_up: false,
            device_handle_ptr: device,
        }
    }

    #[test]
    fn raw_event_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<RawInputEvent>();
    }

    #[test]
    fn consumer_event_without_mapping_is_ignored() {
        // Must not panic and must not queue anything (no observable
        // effect in unit tests).
        process_consumer_event(
            &consumer_event(HidUsage::PlayPause, 0x11),
            HidUsage::PlayPause,
            &empty_lookup(),
        );
    }

    #[test]
    fn consumer_event_with_mapping_runs_the_lookup() {
        let outputs = vec![
            NativeKey {
                modifiers: 0,
                usage: HidUsage::A,
            },
            NativeKey {
                modifiers: 0,
                usage: HidUsage::B,
            },
        ];
        let lookup: Arc<RwLock<dyn Lookup>> = Arc::new(RwLock::new(
            MockLookup::with_global(vec![(HidUsage::PlayPause, outputs)]),
        ));

        // Emission is compiled out of unit tests, so the observable
        // contract is: the lookup runs and the call completes without
        // panicking.
        process_consumer_event(
            &consumer_event(HidUsage::PlayPause, 0x11),
            HidUsage::PlayPause,
            &lookup,
        );
    }

    #[test]
    fn key_up_events_are_not_buffered() {
        let mut event = keyboard_event(HidUsage::A, 0x11);
        event.is_key_up = true;
        process_raw_event(&event, &empty_lookup());
        assert!(match_usage(HidUsage::A).is_none());
    }

    #[test]
    fn keyboard_key_down_is_buffered_for_matching() {
        // A usage unique to this test keeps the shared static isolated
        // from the parallel unit tests.
        process_raw_event(
            &keyboard_event(HidUsage::F9, 0x55),
            &empty_lookup(),
        );
        assert_eq!(match_usage(HidUsage::F9), Some(0x55));
    }

    #[test]
    fn consumer_page_routing_is_consistent() {
        // Sanity: the routing check keys off the consumer page.
        assert_eq!(HidUsage::PlayPause.page(), PAGE_CONSUMER);
    }
}
