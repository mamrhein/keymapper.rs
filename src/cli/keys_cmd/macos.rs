// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS implementation of `keymapper keys probe`.
//!
//! The probe runs standalone (the daemon must not be capturing), but it
//! no longer mirrors the daemon's capture logic: it drives the platform
//! contract's read-only observe mode ([`KeySource::observe`]), whose
//! passive CGEventTap decodes with the very same [`KeyScanner`] the
//! daemon's tap callback feeds — the keycode-to-usage decode and the
//! modifier/CapsLock direction classification therefore have exactly one
//! implementation on macOS.
//!
//! Unlike the other platforms, the tap is session-global: a CGEventTap
//! sees every keyboard and CGEvents expose no originating device (F7a),
//! so the discovered [`KeyboardInfo`] is printed for context but selects
//! nothing.
//!
//! [`KeyboardInfo`]: crate::common::keyboard::KeyboardInfo
//! [`KeyScanner`]: crate::platform::macos::capture::KeyScanner
//! [`KeySource::observe`]: crate::platform::backend::KeySource::observe

use crate::{
    common::keyboard::KeyboardInfo,
    keymap_core::logfmt::Direction,
    platform::{
        MacOsBackend,
        backend::{CapturedKey, KeySource as _},
    },
};

/// Probe for key presses through the platform contract's read-only
/// observe mode.
pub fn probe() {
    let backend = MacOsBackend;

    // Discover keyboards for the header.  Failure or emptiness does not
    // stop the probe: the observe tap does not depend on the
    // enumeration (the tap is session-global), and the platform's
    // `list_keyboards` may answer with the placeholder entry on `ioreg`
    // failure (F8).
    let keyboards = backend.list_keyboards().ok();
    let kb = keyboards
        .as_deref()
        .and_then(<[_]>::first)
        .cloned()
        .unwrap_or_else(|| {
            KeyboardInfo::new(
                "Keyboard".into(),
                String::new(),
                String::new(),
                "session".into(),
                None,
            )
        });

    println!("Probing {} ({})\n", kb.name, kb.device);
    println!("Press keys to see their names and codes.");
    println!("Press Control+C to exit.\n");

    // The observe pump creates the tap (the only fatal step), then
    // delivers every decoded key event until the process is terminated.
    if let Err(e) = backend.observe(&kb, &mut |key| {
        print_captured(&key);
    }) {
        eprintln!("Failed to create event tap: {e}");
        std::process::exit(1);
    }
}

/// Print one decoded key event, key-downs only (presses and modifier
/// presses).
fn print_captured(key: &CapturedKey) {
    if key.direction() != Direction::Down {
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
}
