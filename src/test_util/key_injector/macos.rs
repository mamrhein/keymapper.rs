// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS keyboard injector using a second Karabiner virtual keyboard.
//!
//! The injector opens its own connection to the Karabiner DriverKit daemon
//! and registers a virtual keyboard with the injection identity
//! ([`INJECTION_KEYBOARD_IDENTITY`]).  The daemon under test seizes that
//! keyboard through the same IOKit path it uses for physical keyboards, so
//! injected keystrokes flow through the regular capture pipeline — no
//! CGEventTap, and no feedback loop from the daemon's own output keyboard.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use super::{InjectorError, KeyInjector};
use crate::{
    HidUsage,
    common::hid_usage::PAGE_KEYBOARD,
    platform::{INJECTION_KEYBOARD_IDENTITY, KarabinerClient},
};

/// How long `setup()` waits for the injection keyboard to become ready.
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Inter-event pacing, matching the previous CGEvent-based injector.
const EVENT_PACING: Duration = Duration::from_millis(5);

/// Whether the platform injector can inject the given usage.
///
/// The Karabiner keyboard report carries keyboard-page usage slots only;
/// the consumer report path exists but is not wired into the injector.
pub fn is_injectable(usage: HidUsage) -> bool {
    usage.page() == PAGE_KEYBOARD
}

/// macOS keyboard injector for end-to-end tests.
///
/// Owns a [`KarabinerClient`] connection that registers the injection
/// keyboard.  Held keys are tracked so that every report is a complete
/// snapshot of the keyboard state: HID keyboard reports are not deltas, so
/// a report must carry every key that is currently down.
pub struct MacOSInjector {
    /// The client that owns the injection keyboard.  `None` until
    /// `setup()` succeeds and after `teardown()`.
    client: Option<Arc<KarabinerClient>>,

    /// The usages currently held down.
    held: Mutex<HashSet<HidUsage>>,
}

impl MacOSInjector {
    /// Build the report state for a set of held keys: the modifier byte and
    /// the sorted non-modifier usage ids.
    ///
    /// Modifier keys travel in the report's modifier byte rather than the
    /// usage slots — the same convention the daemon's output path uses.
    fn report_for(held: &HashSet<HidUsage>) -> (u8, Vec<u16>) {
        let mut modifiers = 0u8;
        let mut usages: Vec<u16> = Vec::new();
        for usage in held {
            if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(*usage) {
                modifiers |= 1 << bit;
            } else {
                usages.push(usage.id());
            }
        }
        usages.sort_unstable();
        (modifiers, usages)
    }

    /// Update the held set and send the new state.
    fn inject(
        &self,
        usage: HidUsage,
        is_down: bool,
    ) -> Result<(), InjectorError> {
        // The Karabiner keyboard report carries keyboard-page usage slots
        // only; consumer-page usages cannot be injected through it.
        if usage.page() != PAGE_KEYBOARD {
            return Err(InjectorError::NotSupported(format!(
                "consumer page usage {} cannot be carried by the keyboard \
                 report",
                usage.as_str()
            )));
        }

        let (modifiers, usages) = {
            let mut held = self.held.lock().unwrap();
            if is_down {
                held.insert(usage);
            } else {
                held.remove(&usage);
            }
            Self::report_for(&held)
        };

        let client = self.client.as_ref().ok_or_else(|| {
            InjectorError::InjectionFailed(
                "the injector is not set up".to_string(),
            )
        })?;

        client
            .send_keyboard_report(modifiers, &usages)
            .map_err(|e| InjectorError::InjectionFailed(e.to_string()))?;
        thread::sleep(EVENT_PACING);
        Ok(())
    }
}

impl KeyInjector for MacOSInjector {
    fn new() -> Result<Option<Self>, InjectorError> {
        // The prerequisite is root access to the Karabiner service socket,
        // not Accessibility: the injection keyboard is a DriverKit device,
        // and only root may talk to the daemon that owns it.
        KarabinerClient::probe_socket().map_err(|e| {
            InjectorError::PermissionDenied(format!(
                "cannot reach the Karabiner service socket ({e}); is the \
                 Karabiner-VirtualHIDDevice-Daemon running, and is this \
                 process running as root?"
            ))
        })?;

        Ok(Some(Self {
            client: None,
            held: Mutex::new(HashSet::new()),
        }))
    }

    fn setup(&mut self) -> Result<(), InjectorError> {
        let client = KarabinerClient::connect(INJECTION_KEYBOARD_IDENTITY)
            .map_err(|e| InjectorError::DeviceCreationFailed(e.to_string()))?;

        if !client.wait_ready(SETUP_TIMEOUT) {
            return Err(InjectorError::DeviceCreationFailed(format!(
                "the injection keyboard did not become ready within {} s",
                SETUP_TIMEOUT.as_secs()
            )));
        }

        self.client = Some(Arc::new(client));
        Ok(())
    }

    fn inject_key_down(&self, usage: HidUsage) -> Result<(), InjectorError> {
        self.inject(usage, true)
    }

    fn inject_key_up(&self, usage: HidUsage) -> Result<(), InjectorError> {
        self.inject(usage, false)
    }

    fn teardown(&mut self) {
        // Dropping the client closes the socket; the daemon destroys the
        // injection keyboard node when the connection goes away.
        self.client = None;
        self.held.lock().unwrap().clear();
    }
}

impl Drop for MacOSInjector {
    fn drop(&mut self) {
        self.teardown();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injector_new_returns_some_or_permission_error() {
        let result = MacOSInjector::new();
        assert!(
            result.is_ok()
                || matches!(
                    result.as_ref().err().unwrap(),
                    InjectorError::PermissionDenied(..)
                ),
            "Injector::new() returned {:?}",
            result.err()
        );
    }

    #[test]
    fn report_for_empty() {
        let held: HashSet<HidUsage> = HashSet::new();
        let (modifiers, usages) = MacOSInjector::report_for(&held);
        assert_eq!(modifiers, 0);
        assert!(usages.is_empty());
    }

    #[test]
    fn report_for_splits_modifiers_and_usages() {
        let mut held: HashSet<HidUsage> = HashSet::new();
        held.insert(HidUsage::LeftShift);
        held.insert(HidUsage::B);

        let (modifiers, usages) = MacOSInjector::report_for(&held);
        // LeftShift is modifier bit 1; B is usage 0x05.
        assert_eq!(modifiers, 1 << 1);
        assert_eq!(usages, vec![0x05]);
    }

    #[test]
    fn report_for_sorts_usages() {
        let mut held: HashSet<HidUsage> = HashSet::new();
        held.insert(HidUsage::Z); // 0x1D
        held.insert(HidUsage::A); // 0x04

        let (modifiers, usages) = MacOSInjector::report_for(&held);
        assert_eq!(modifiers, 0);
        assert_eq!(usages, vec![0x04, 0x1D]);
    }

    #[test]
    fn injector_error_display() {
        let e = InjectorError::PermissionDenied("test".to_string());
        assert!(format!("{e}").contains("permission denied"));

        let e = InjectorError::DeviceCreationFailed("test".to_string());
        assert!(format!("{e}").contains("device creation failed"));

        let e = InjectorError::InjectionFailed("test".to_string());
        assert!(format!("{e}").contains("injection failed"));

        let e = InjectorError::NotSupported("test".to_string());
        assert!(format!("{e}").contains("not supported"));
    }

    #[test]
    fn injector_error_is_std_error() {
        let e: Box<dyn std::error::Error> =
            Box::new(InjectorError::NotSupported("test".into()));
        assert!(!e.to_string().is_empty());
    }

    #[test]
    fn is_injectable_excludes_consumer_page() {
        // Consumer page usages have no keyboard-report slot.
        assert!(!is_injectable(HidUsage::PlayPause));
        assert!(!is_injectable(HidUsage::VolumeUp));
        assert!(!is_injectable(HidUsage::Mute));

        // Keyboard page usages are injectable.
        assert!(is_injectable(HidUsage::A));
        assert!(is_injectable(HidUsage::LeftControl));
    }
}
