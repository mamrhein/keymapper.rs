// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux implementation of `keymapper keys probe`.
//!
//! The probe runs standalone (the daemon must not be capturing), but it
//! no longer mirrors the daemon's capture logic: it opens the first
//! discovered keyboard through the platform contract's read-only observe
//! mode ([`KeySource::observe`]), which decodes with the very same
//! scanner the daemon's grabbed capture path feeds.  The
//! `MSC_SCAN` buffering and the HID decode therefore have exactly one
//! implementation on Linux.
//!
//! [`KeySource::observe`]: crate::platform::backend::KeySource::observe

use crate::platform::{
    LinuxBackend,
    backend::{CapturedKey, KeySource as _},
};

/// Probe for key presses by reading from an evdev keyboard device.
///
/// Uses the first keyboard returned by the shared discovery to avoid
/// duplicating udev enumeration logic.
pub fn probe() {
    let backend = LinuxBackend;

    // Discover keyboards and pick the first one for probing.
    let keyboards = backend.list_keyboards().unwrap_or_else(|e| {
        eprintln!("Failed to discover keyboards: {e}");
        std::process::exit(1);
    });

    if keyboards.is_empty() {
        eprintln!("No keyboard devices found.");
        std::process::exit(1);
    }

    let kb = &keyboards[0];

    println!("Probing {} ({})\n", kb.name, kb.device);
    println!("Press keys to see their names and codes.");
    println!("Press Control+C to exit.\n");

    // The observe pump opens the device (the only fatal step), then
    // delivers every decoded key event until the process is terminated.
    if let Err(e) = backend.observe(kb, &mut |key: CapturedKey| {
        // Print only on key down.
        if key.value != 1 {
            return;
        }

        let (name, code_str) = match key.usage {
            Some(u) => (u.as_str().to_string(), format!("0x{:02X}", u.id())),
            None => (
                format!("Unknown({})", key.native),
                format!("{}", key.native),
            ),
        };

        println!("{name}: {code_str}");
    }) {
        eprintln!("Failed to open keyboard device: {e}");
        std::process::exit(1);
    }
}
