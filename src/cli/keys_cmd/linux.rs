// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux implementation of `keymapper keys probe`.

use std::time::Duration;

use evdev::{Device, EventType, MiscCode};

use crate::{
    common::hid_usage::HidUsage, platform::hid_translate::keycode_to_hid_usage,
};

/// Probe for key presses by reading from an evdev keyboard device.
///
/// Uses the first keyboard returned by the shared discovery to avoid
//  duplicating udev enumeration logic.
pub fn probe() {
    // Discover keyboards and pick the first one for probing.
    let keyboards = crate::platform::list_keyboards().unwrap_or_else(|e| {
        eprintln!("Failed to discover keyboards: {e}");
        std::process::exit(1);
    });

    if keyboards.is_empty() {
        eprintln!("No keyboard devices found.");
        std::process::exit(1);
    }

    let kb = &keyboards[0];
    let mut device = Device::open(&kb.device).unwrap_or_else(|e| {
        eprintln!("Failed to open keyboard device: {e}");
        std::process::exit(1);
    });

    println!("Probing {} ({})\n", kb.name, kb.device);

    println!("Press keys to see their names and codes.");
    println!("Press Control+C to exit.\n");

    // MSC_SCAN values are buffered until the next EV_KEY event, mirroring
    // the daemon's capture logic.
    let mut pending_scan: Option<u32> = None;

    loop {
        match device.fetch_events() {
            Ok(events) => {
                for event in events {
                    // MSC_SCAN events carry the raw HID usage
                    // `(page << 16) | id` and precede the EV_KEY event of
                    // the same key press.
                    if event.event_type() == EventType::MISC
                        && event.code() == MiscCode::MSC_SCAN.0
                    {
                        pending_scan = Some(event.value() as u32);
                        continue;
                    }

                    if event.event_type() != EventType::KEY {
                        continue;
                    }

                    let code = event.code();
                    let value = event.value();

                    // Print only on key down.
                    if value != 1 {
                        continue;
                    }

                    // Prefer the raw HID usage from MSC_SCAN; fall back to
                    // the EV_KEY reverse lookup for devices that do not
                    // emit MSC_SCAN.
                    let usage = pending_scan
                        .take()
                        .and_then(HidUsage::from_code)
                        .or_else(|| keycode_to_hid_usage(code));

                    let (name, code_str) = match usage {
                        Some(u) => (
                            u.as_str().to_string(),
                            format!("0x{:02X}", u.id()),
                        ),
                        None => {
                            (format!("Unknown({code})"), format!("{code}"))
                        }
                    };

                    println!("{name}: {code_str}");
                }
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
