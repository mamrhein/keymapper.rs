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
pub fn probe() {
    println!("Press keys to see their names and codes.");
    println!("Press Control+Escape to exit.\n");

    let mut device = find_keyboard().unwrap_or_else(|e| {
        eprintln!("Failed to open keyboard device: {e}");
        std::process::exit(1);
    });

    // Don't grab — we just want to observe, not intercept.
    let mut ctrl_pressed = false;

    loop {
        match device.fetch_events() {
            Ok(events) => {
                for event in events {
                    if event.event_type() == EventType::KEY {
                        let code = event.code();
                        let value = event.value();
                        let is_key_down = value == 1;

                        // Track Ctrl state for exit detection.
                        if code == Key::LeftControl.as_native()
                            || code == Key::RightControl.as_native()
                        {
                            ctrl_pressed = is_key_down;
                        }

                        // Check for Ctrl+Escape exit condition.
                        if is_key_down
                            && code == Key::Escape.as_native()
                            && ctrl_pressed
                        {
                            return;
                        }

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

/// Find a suitable keyboard device for probing.
fn find_keyboard() -> Result<Device, Box<dyn std::error::Error>> {
    // Try common keyboard device paths.
    let candidates = [
        "/dev/input/event0",
        "/dev/input/event1",
        "/dev/input/event2",
        "/dev/input/event3",
        "/dev/input/event4",
        "/dev/input/event5",
    ];

    for path in &candidates {
        if let Ok(device) = Device::open(path) {
            // Check that the device supports keyboard events.
            if device.supported_events().contains(EventType::KEY) {
                return Ok(device);
            }
        }
    }

    Err("No keyboard device found in /dev/input/event*".into())
}
