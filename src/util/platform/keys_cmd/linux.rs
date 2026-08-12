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

use evdev::{Device, EventType};

use crate::platform::Key;

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

    loop {
        match device.fetch_events() {
            Ok(events) => {
                for event in events {
                    if event.event_type() == EventType::KEY {
                        let code = event.code();
                        let value = event.value();
                        let is_key_down = value == 1;

                        // Print only on key down.
                        if is_key_down {
                            let (name, code_str) = if let Some(key) =
                                Key::from_native(code)
                            {
                                (
                                    key.as_str().to_string(),
                                    format!("{}", key.as_native()),
                                )
                            } else {
                                (format!("Unknown({code})"), format!("{code}"))
                            };

                            println!("{name}: {code_str}");
                        }
                    }
                }
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
