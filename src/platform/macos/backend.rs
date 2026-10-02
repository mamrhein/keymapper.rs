// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The macOS implementation of the cross-platform [`KeySource`] contract.
//!
//! Thin glue onto the macOS capture stack: enumeration delegates to
//! [`keyboard`] (`ioreg`), observation to a passive CGEventTap driven by
//! the shared [`KeyScanner`], and the mapping runtime to [`mapping`].
//!
//! The echo-suppression predicate answers `false` for every device, as
//! the contract allows for platforms that do not model their output
//! device as an enumerable input device: the DriverKit virtual keyboard
//! is not listed by `ioreg`, so a name-based predicate has nothing to
//! match — self-echo is instead suppressed capture-side by the
//! predictive [`EchoTracker`].  Built-in hardware keyboards are never
//! output devices and are never skipped.
//!
//! The [`list_keyboards`] and [`start_mapping`] free functions are the
//! exports that [`crate::platform`] re-exports: on macOS they are thin
//! shims that drive the contract, so the trait — not a parallel set of
//! free functions — is the definition of the platform boundary.  (The
//! emission half of the contract, [`MacOsEmitter`], lives in [`mapping`]
//! beside the IPC sender and echo tracker it owns.)
//!
//! [`EchoTracker`]: super::mapping::EchoTracker
//! [`MacOsEmitter`]: super::mapping::MacOsEmitter

use std::{error::Error, sync::Arc};

use parking_lot::RwLock;

use crate::{
    common::keyboard::{KeyboardInfo, KeyboardSpecifier},
    keymap_core::lookup::Lookup,
    platform::{
        backend::{CapturedKey, KeySource},
        macos::{capture, keyboard, mapping},
    },
};

/// The macOS device-I/O backend.
pub(crate) struct MacOsBackend;

impl KeySource for MacOsBackend {
    fn list_keyboards(&self) -> Result<Vec<KeyboardInfo>, Box<dyn Error>> {
        keyboard::list_keyboards()
    }

    /// Echo suppression on macOS is predictive ([`EchoTracker`]), not
    /// enumerable: no listed device is the daemon's output device, so
    /// nothing is ever skipped by name.
    ///
    /// [`EchoTracker`]: super::mapping::EchoTracker
    fn is_output_device(&self, _name: &str) -> bool {
        false
    }

    fn observe(
        &self,
        keyboard: &KeyboardInfo,
        on_key: &mut dyn FnMut(CapturedKey),
    ) -> Result<(), Box<dyn Error>> {
        capture::observe(keyboard, on_key)
    }

    fn start_mapping(
        &self,
        lookup: Arc<RwLock<dyn Lookup>>,
        keyboard_filter: Option<Vec<KeyboardSpecifier>>,
    ) -> Result<(), Box<dyn Error>> {
        mapping::start_mapping(lookup, keyboard_filter)
    }
}

/// Enumerate all keyboard input devices on macOS.
///
/// The [`crate::platform`] export; drives the contract through
/// [`MacOsBackend::list_keyboards`].
pub fn list_keyboards() -> Result<Vec<KeyboardInfo>, Box<dyn Error>> {
    MacOsBackend.list_keyboards()
}

/// Start the macOS capture → map → emit runtime.
///
/// The [`crate::platform`] export; drives the contract through
/// [`MacOsBackend::start_mapping`].
pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    keyboard_filter: Option<Vec<KeyboardSpecifier>>,
) -> Result<(), Box<dyn Error>> {
    MacOsBackend.start_mapping(lookup, keyboard_filter)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The DriverKit virtual keyboard is not listed by `ioreg` and the
    /// Karabiner virtual device is not the daemon's, so no enumerated
    /// name is ever the output device.
    #[test]
    fn no_enumerated_device_is_the_output_device() {
        assert!(!MacOsBackend.is_output_device("Magic Keyboard"));
        assert!(!MacOsBackend.is_output_device("Apple Internal Keyboard"));
    }
}
