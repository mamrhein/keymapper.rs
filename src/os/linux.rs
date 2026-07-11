// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::sync::Arc;

use evdev::{Device, Key};
use parking_lot::RwLock;

use crate::{mapping_cache::NativeAction, state::Lookup};

pub(crate) fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Dynamic path discovery should ideally replace this static file node
    let device_path = "/dev/input/event3";
    let mut raw_device = Device::open(device_path)?;
    raw_device.grab()?;

    let mut virtual_device = uinput::default()?
        .name("CrossPlatform_Virtual_Keyboard")?
        .event(uinput::event::Keyboard::All)?
        .create()?;

    std::thread::sleep(std::Duration::from_millis(200));
    println!("Linux uinput loop virtual keyboard ready.");

    loop {
        for event in raw_device.fetch_events()? {
            if event.event_type() == evdev::EventType::KEY {
                // event.code() returns u16 — matches NativeKey directly.
                let code = event.code();
                let value = event.value(); // 1 = Down, 0 = Up, 2 = Repeat

                let guard = lookup.read();
                let current_app = guard.active_app().to_lowercase();
                let active_action = guard
                    .for_app(&current_app, code)
                    .or_else(|| guard.global(code));

                if let Some(action) = active_action {
                    match action {
                        NativeAction::RemapTo(target) => {
                            let key = Key::new(*target);
                            if value == 1 {
                                virtual_device.press(&key)?;
                            } else if value == 0 {
                                virtual_device.release(&key)?;
                            }
                            virtual_device.synchronize()?;
                        }
                        NativeAction::Shortcut(targets) => {
                            if value == 1 {
                                for t in targets.iter() {
                                    let key = Key::new(*t);
                                    virtual_device.press(&key)?;
                                }
                            } else if value == 0 {
                                for t in targets.iter().rev() {
                                    let key = Key::new(*t);
                                    virtual_device.release(&key)?;
                                }
                            }
                            virtual_device.synchronize()?;
                        }
                    }
                } else {
                    // Passthrough
                    let key = Key::new(code);
                    if value == 1 {
                        virtual_device.press(&key)?;
                    } else if value == 0 {
                        virtual_device.release(&key)?;
                    }
                    virtual_device.synchronize()?;
                }
            }
        }
    }
}
