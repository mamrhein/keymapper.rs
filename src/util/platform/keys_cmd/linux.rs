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

use evdev::EventType;

use crate::platform::{Key, find_keyboard_device};

/// Probe for key presses by reading from an evdev keyboard device.
pub fn probe() {
    let (mut device, path) = find_keyboard_device().unwrap_or_else(|e| {
        eprintln!("Failed to open keyboard device: {e}");
        std::process::exit(1);
    });

    println!(
        "Probing {} ({})\n",
        device.name().unwrap_or("<unknown>"),
        path
    );

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
