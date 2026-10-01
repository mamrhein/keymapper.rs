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

    /// Echo suppression: the daemon's uinput output device is tagged as
    /// a keyboard by udev but must never be grabbed, or the daemon's
    /// emitted events would feed back into its own input loop.
    fn is_output_device(&self, name: &str) -> bool {
        name == mapping::VIRTUAL_KEYBOARD_NAME
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
    use crate::platform::linux::mapping::VIRTUAL_KEYBOARD_NAME;

    #[test]
    fn only_the_virtual_keyboard_is_the_output_device() {
        assert!(LinuxBackend.is_output_device(VIRTUAL_KEYBOARD_NAME));
        assert!(!LinuxBackend.is_output_device("Some physical keyboard"));
    }
}
