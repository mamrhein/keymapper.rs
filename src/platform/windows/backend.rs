// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The Windows implementation of the cross-platform [`KeySource`]
//! contract.
//!
//! Thin glue onto the Windows capture stack: enumeration delegates to
//! [`keyboard`] (SetupAPI), observation to a passive
//! `WH_KEYBOARD_LL` hook driven by the shared [`KeyScanner`], and the
//! mapping runtime to [`mapping`].
//!
//! The echo-suppression predicate answers `false` for every device,
//! as the contract allows for platforms that do not model their output
//! device as an enumerable input device: emission goes through
//! `SendInput` into the OS input stack and never creates a device that
//! enumeration could list — self-echo is instead suppressed
//! capture-side by the [`INJECTED_TAG`](super::INJECTED_TAG) filter in
//! the daemon's hook proc.
//!
//! The [`list_keyboards`] and [`start_mapping`] free functions are the
//! exports that [`crate::platform`] re-exports: on Windows they are
//! thin shims that drive the contract, so the trait — not a parallel
//! set of free functions — is the definition of the platform
//! boundary.  (The emission half of the contract, [`WindowsEmitter`],
//! lives in [`mapping`] beside the `SendInput` primitives it maps
//! actions onto.)
//!
//! [`WindowsEmitter`]: super::mapping::WindowsEmitter

use std::{error::Error, sync::Arc};

use parking_lot::RwLock;

use crate::{
    common::keyboard::{KeyboardInfo, KeyboardSpecifier},
    keymap_core::lookup::Lookup,
    platform::{
        backend::{CapturedKey, KeySource},
        windows::{capture, keyboard, mapping},
    },
};

/// The Windows device-I/O backend.
pub(crate) struct WindowsBackend;

impl KeySource for WindowsBackend {
    fn list_keyboards(&self) -> Result<Vec<KeyboardInfo>, Box<dyn Error>> {
        keyboard::list_keyboards()
    }

    /// Echo suppression on Windows is the injected-event tag
    /// ([`INJECTED_TAG`](super::INJECTED_TAG)) inside the hook, not
    /// enumerable: `SendInput` never creates a listed device, so
    /// nothing is ever skipped by name.
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

/// Enumerate all keyboard input devices on Windows.
///
/// The [`crate::platform`] export; drives the contract through
/// [`WindowsBackend::list_keyboards`].
pub fn list_keyboards() -> Result<Vec<KeyboardInfo>, Box<dyn Error>> {
    WindowsBackend.list_keyboards()
}

/// Start the Windows capture → map → emit runtime.
///
/// The [`crate::platform`] export; drives the contract through
/// [`WindowsBackend::start_mapping`].
pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    keyboard_filter: Option<Vec<KeyboardSpecifier>>,
) -> Result<(), Box<dyn Error>> {
    WindowsBackend.start_mapping(lookup, keyboard_filter)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SendInput` emissions never create a listed device, so no
    /// enumerated name is ever the output device.
    #[test]
    fn no_enumerated_device_is_the_output_device() {
        assert!(!WindowsBackend.is_output_device("HID Keyboard Device"));
        assert!(!WindowsBackend.is_output_device("keymapper virtual"));
    }
}
