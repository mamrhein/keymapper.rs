// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The Linux implementation of the cross-platform [`KeySource`] contract.
//!
//! Thin glue onto the Linux capture stack: enumeration delegates to
//! [`keyboard`] (udev), observation to the shared [`KeyScanner`] over an
//! un-grabbed device handle, the mapping runtime to [`mapping`], and the
//! echo-suppression predicate names the daemon's uinput output device
//! ([`VIRTUAL_KEYBOARD_NAME`]).
//!
//! The [`list_keyboards`] and [`start_mapping`] free functions are the
//! exports that [`crate::platform`] re-exports: on Linux they are thin
//! shims that drive the contract, so the trait — not a parallel set of
//! free functions — is the definition of the platform boundary.  (The
//! emission half of the contract, [`LinuxEmitter`], lives in [`mapping`]
//! beside the uinput device it owns.)
//!
//! [`LinuxEmitter`]: super::mapping::LinuxEmitter

use std::{error::Error, sync::Arc, thread, time::Duration};

use evdev::Device;
use parking_lot::RwLock;

use crate::{
    common::keyboard::{KeyboardInfo, KeyboardSpecifier},
    keymap_core::lookup::Lookup,
    platform::{
        backend::{CapturedKey, KeySource},
        linux::{capture::KeyScanner, keyboard, mapping},
    },
};

/// The Linux device-I/O backend.
pub(crate) struct LinuxBackend;

impl KeySource for LinuxBackend {
    fn list_keyboards(&self) -> Result<Vec<KeyboardInfo>, Box<dyn Error>> {
        keyboard::list_keyboards()
    }

    /// Echo suppression: the daemon's uinput output device must never be
    /// grabbed, or its emitted events would feed back into its own input
    /// loop.  Identity is the kernel-assigned devnode (R3-11), not the
    /// device name string, so a process that creates a device with the
    /// same name cannot force the daemon to skip it.
    fn is_output_device(&self, keyboard: &KeyboardInfo) -> bool {
        keyboard::is_own_output_device(&keyboard.device)
    }

    fn observe(
        &self,
        keyboard: &KeyboardInfo,
        on_key: &mut dyn FnMut(CapturedKey),
    ) -> Result<(), Box<dyn Error>> {
        // Read-only observe mode: the device is opened but never
        // grabbed, so the probe shares it with the system's own readers
        // and runs while the daemon does not.  Decoding goes through the
        // same scanner the daemon's capture path feeds, so probe and
        // daemon cannot drift.
        let mut device = Device::open(&keyboard.device)?;
        let mut scanner = KeyScanner::new();
        loop {
            match device.fetch_events() {
                Ok(events) => {
                    for event in events {
                        if let Some(key) = scanner.on_event(event) {
                            on_key(key);
                        }
                    }
                }
                Err(_) => {
                    // A transient read error (device busy, momentary
                    // unavailability) is retried rather than treated as
                    // fatal; the pump ends only with the process.
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    fn start_mapping(
        &self,
        lookup: Arc<RwLock<dyn Lookup>>,
        keyboard_filter: Option<Vec<KeyboardSpecifier>>,
    ) -> Result<(), Box<dyn Error>> {
        mapping::start_mapping(lookup, keyboard_filter)
    }
}

/// Enumerate all keyboard input devices on the system.
///
/// The [`crate::platform`] export; drives the contract through
/// [`LinuxBackend::list_keyboards`].
pub fn list_keyboards() -> Result<Vec<KeyboardInfo>, Box<dyn Error>> {
    LinuxBackend.list_keyboards()
}

/// Start the Linux capture → map → emit runtime.
///
/// The [`crate::platform`] export; drives the contract through
/// [`LinuxBackend::start_mapping`].
pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    keyboard_filter: Option<Vec<KeyboardSpecifier>>,
) -> Result<(), Box<dyn Error>> {
    LinuxBackend.start_mapping(lookup, keyboard_filter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::linux::keyboard;

    /// Identity is the kernel-assigned devnode, not the device name (R3-11).
    /// A device that announces itself as `CrossPlatform_Virtual_Keyboard`
    /// but lives at a different devnode is NOT treated as the output device
    /// — the daemon would grab it and remap its events instead of letting
    /// them pass through unmapped.
    #[test]
    fn is_output_device_keys_on_devnode_not_name() {
        keyboard::set_own_device_path("/dev/input/event42".to_string());
        let own = KeyboardInfo::new(
            "CrossPlatform_Virtual_Keyboard".into(),
            String::new(),
            String::new(),
            "/dev/input/event42".to_string(),
            None,
        );
        let spoofed = KeyboardInfo::new(
            "CrossPlatform_Virtual_Keyboard".into(),
            String::new(),
            String::new(),
            "/dev/input/event7".to_string(),
            None,
        );
        let real = KeyboardInfo::new(
            "Magic Keyboard".into(),
            String::new(),
            String::new(),
            "/dev/input/event3".to_string(),
            None,
        );
        assert!(LinuxBackend.is_output_device(&own));
        assert!(!LinuxBackend.is_output_device(&spoofed));
        assert!(!LinuxBackend.is_output_device(&real));
        keyboard::clear_own_device_path_for_testing();
    }

    /// When the own-device path is not yet set (the startup enumeration runs
    /// before the `VirtualDevice` is created), `is_output_device` is a safe
    /// no-op: every devnode is treated as a real input device.
    #[test]
    fn startup_path_is_a_safe_noop_when_own_path_unset() {
        keyboard::clear_own_device_path_for_testing();
        let kb = KeyboardInfo::new(
            "CrossPlatform_Virtual_Keyboard".into(),
            String::new(),
            String::new(),
            "/dev/input/event42".to_string(),
            None,
        );
        assert!(!LinuxBackend.is_output_device(&kb));
    }

    /// Startup-path unit test (R3-11): the daemon's own uinput device is
    /// never grabbed, even when it appears in the enumeration.  Simulates
    /// the `start_mapping` filter pipeline: global-specifier filter followed
    /// by the `is_output_device` echo-suppression guard.
    #[test]
    fn startup_path_excludes_own_uinput_device() {
        use crate::common::keyboard::filter_keyboards_by_specifiers;

        keyboard::set_own_device_path("/dev/input/event42".to_string());
        let keyboards = vec![
            KeyboardInfo::new(
                "Magic Keyboard".into(),
                "Apple".into(),
                "0x05ac:0xa25a".into(),
                "/dev/input/event3".to_string(),
                Some("USB".to_string()),
            ),
            KeyboardInfo::new(
                "CrossPlatform_Virtual_Keyboard".into(),
                String::new(),
                String::new(),
                "/dev/input/event42".to_string(),
                None,
            ),
        ];
        // Replicate the startup-path filter: specifier filter (no filter =
        // pass-through) + output-device check.
        let grabbed: Vec<KeyboardInfo> =
            filter_keyboards_by_specifiers(&keyboards, None)
                .into_iter()
                .filter(|kb| !LinuxBackend.is_output_device(kb))
                .collect();
        assert_eq!(grabbed.len(), 1);
        assert_eq!(grabbed[0].name, "Magic Keyboard");
        keyboard::clear_own_device_path_for_testing();
    }
}
