// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::sync::Arc;
use parking_lot::RwLock;
use crate::RuntimeState;
use crate::mapping_cache::NativeAction;
use evdev::{Device, Key};
use uinput::event::keyboard;

pub(crate) fn start_mapping(state: Arc<RwLock<RuntimeState>>) -> Result<(), Box<dyn std::error::Error>> {
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
                let code = event.code() as u32;
                let value = event.value(); // 1 = Down, 0 = Up, 2 = Repeat

                let state_guard = state.read();
                let current_app = state_guard.active_app.to_lowercase();

                let mut active_action = state_guard.lookup_cache.process_map.get(&current_app).and_then(|m| m.get(&code));
                if active_action.is_none() {
                    active_action = state_guard.lookup_cache.global_map.get(&code);
                }

                if let Some(action) = active_action {
                    match action {
                        NativeAction::RemapTo(target) => {
                            let key: uinput::event::keyboard::Key = unsafe { std::mem::transmute(*target as i32) };
                            if value == 1 { virtual_device.press(&key)?; }
                            else if value == 0 { virtual_device.release(&key)?; }
                            virtual_device.synchronize()?;
                        }
                        NativeAction::Shortcut(targets) => {
                            if value == 1 {
                                for t in targets {
                                    let key: uinput::event::keyboard::Key = unsafe { std::mem::transmute(*t as i32) };
                                    virtual_device.press(&key)?;
                                }
                            } else if value == 0 {
                                for t in targets.iter().rev() {
                                    let key: uinput::event::keyboard::Key = unsafe { std::mem::transmute(*t as i32) };
                                    virtual_device.release(&key)?;
                                }
                            }
                            virtual_device.synchronize()?;
                        }
                    }
                } else {
                    // Passthrough
                    let key: uinput::event::keyboard::Key = unsafe { std::mem::transmute(code as i32) };
                    if value == 1 { virtual_device.press(&key)?; }
                    else if value == 0 { virtual_device.release(&key)?; }
                    virtual_device.synchronize()?;
                }
            }
        }
    }
}
