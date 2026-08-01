// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use evdev::{Device, EventType, KeyCode};
use parking_lot::RwLock;
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::flag::register;
use udev::Enumerator;

use crate::{
    common::modifier::ModifierRole,
    daemon::{mapping_cache::NativeKey, state::Lookup},
};

use super::key::Key;

// ---------------------------------------------------------------------------
// Modifier handling
// ---------------------------------------------------------------------------

/// Map a raw evdev keycode to its modifier bit position via the shared
/// `ModifierRole` type.
fn keycode_to_modifier_bit(code: u16) -> Option<u8> {
    let role = match code {
        29 => ModifierRole::LeftControl, // KEY_LEFTCTRL
        97 => ModifierRole::RightControl, // KEY_RIGHTCTRL
        42 => ModifierRole::LeftShift,   // KEY_LEFTSHIFT
        54 => ModifierRole::RightShift,  // KEY_RIGHTSHIFT
        56 => ModifierRole::LeftAlt,     // KEY_LEFTALT
        100 => ModifierRole::RightAlt,   // KEY_RIGHTALT
        125 => ModifierRole::LeftCommand, // KEY_LEFTMETA
        126 => ModifierRole::RightCommand, // KEY_RIGHTMETA
        _ => return None,
    };
    Some(role.bit())
}

/// Map a modifier bit position back to the native evdev keycode for emission.
fn modifier_bit_to_code(bit: u8) -> Option<u16> {
    let role = ModifierRole::try_from_bit(bit)?;
    let key = match role {
        ModifierRole::LeftControl => Key::LeftControl,
        ModifierRole::RightControl => Key::RightControl,
        ModifierRole::LeftShift => Key::LeftShift,
        ModifierRole::RightShift => Key::RightShift,
        ModifierRole::LeftAlt => Key::LeftAlt,
        ModifierRole::RightAlt => Key::RightAlt,
        ModifierRole::LeftCommand => Key::LeftCommand,
        ModifierRole::RightCommand => Key::RightCommand,
    };
    Some(key.as_native())
}

fn emit_key_event(
    device: &mut uinput::Device,
    native_key: &NativeKey,
) -> Result<(), Box<dyn std::error::Error>> {
    // Track all pressed codes so they can be released on failure.
    let mut pressed: Vec<u16> = Vec::new();

    // Helper to release any keys that were successfully pressed.
    let cleanup = |dev: &mut uinput::Device, codes: &[u16]| {
        for code in codes.iter().rev() {
            if let Err(e) = dev.write(EventType::KEY.0 as _, *code as _, 0) {
                eprintln!("warning: failed to release key {code}: {e}");
            }
            let _ = dev.synchronize();
            thread::sleep(Duration::from_millis(1));
        }
    };

    // Press modifiers.
    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1
            && let Some(code) = modifier_bit_to_code(bit)
        {
            if let Err(e) = device.write(EventType::KEY.0 as _, code as _, 1) {
                cleanup(device, &pressed);
                return Err(e.into());
            }
            pressed.push(code);
            if let Err(e) = device.synchronize() {
                cleanup(device, &pressed);
                return Err(e.into());
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    // Press and release the base key.
    if let Err(e) =
        device.write(EventType::KEY.0 as _, native_key.base as _, 1)
    {
        cleanup(device, &pressed);
        return Err(e.into());
    }
    pressed.push(native_key.base);
    if let Err(e) = device.synchronize() {
        cleanup(device, &pressed);
        return Err(e.into());
    }
    thread::sleep(Duration::from_millis(1));

    if let Err(e) =
        device.write(EventType::KEY.0 as _, native_key.base as _, 0)
    {
        cleanup(device, &pressed);
        return Err(e.into());
    }
    pressed.pop();
    if let Err(e) = device.synchronize() {
        cleanup(device, &pressed);
        return Err(e.into());
    }
    thread::sleep(Duration::from_millis(1));

    // Release modifiers in reverse order.
    for code in pressed.into_iter().rev() {
        if let Err(e) = device.write(EventType::KEY.0 as _, code as _, 0) {
            return Err(e.into());
        }
        if let Err(e) = device.synchronize() {
            return Err(e.into());
        }
        thread::sleep(Duration::from_millis(1));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// evdev event loop
// ---------------------------------------------------------------------------

const DEFAULT_SEAT: &str = "seat0";

/// Determine the seat of the current user session.
///
/// Strategy (first match wins):
/// 1. `XDG_SEAT` environment variable.
/// 2. Parse the session file under `/run/systemd/sessions/<id>` and read the `SEAT=` line.
/// 3. Fallback to `seat0`.
fn determine_seat() -> String {
    // Check the environment first.
    if let Ok(seat) = std::env::var("XDG_SEAT")
        && !seat.is_empty()
    {
        return seat;
    }

    // Resolve the session id and look up the seat in its systemd session file.
    if let Ok(session_id) = std::fs::read_to_string("/proc/self/sessionid") {
        let session_id = session_id.trim();
        let path = format!("/run/systemd/sessions/{session_id}");
        if let Ok(contents) = std::fs::read_to_string(&path) {
            for line in contents.lines() {
                if let Some(seat) = line.strip_prefix("SEAT=")
                    && !seat.is_empty()
                {
                    return seat.to_string();
                }
            }
        }
    }

    // Default fallback.
    DEFAULT_SEAT.to_string()
}

/// Find the first keyboard input device that belongs to the current user seat.
///
/// This uses `udevrs` to enumerate devices tagged for the seat and filtered to
/// keyboards.  If udev enumeration fails or returns no candidates it falls back
/// to the legacy approach of scanning `/dev/input/event*`.
pub(crate) fn find_keyboard_device()
-> Result<Device, Box<dyn std::error::Error>> {
    let seat = determine_seat();

    // Try seat-aware udev enumeration first.
    match find_keyboard_device_udev(&seat) {
        Ok(device) => Ok(device),
        Err(e) => {
            eprintln!(
                "Warning: udev keyboard discovery failed ({e}), falling back to \
                 /dev/input scan"
            );
            find_keyboard_device_fallback()
        }
    }
}

/// Find a keyboard device for `seat` using udev.
fn find_keyboard_device_udev(
    seat: &str,
) -> Result<Device, Box<dyn std::error::Error>> {
    let mut enumerator = Enumerator::new()?;

    enumerator.match_subsystem("input")?;
    enumerator.match_property("ID_INPUT_KEYBOARD", "1")?;
    enumerator.scan_devices()?;

    for udev_device in enumerator.scan_devices()? {
        // Note: Only seats other than 'seat0' are tagged with 'ID_SEAT'.
        let dev_seat = udev_device
            .property_value("ID_SEAT")
            .map(|s| s.to_string_lossy())
            .unwrap_or(DEFAULT_SEAT.into());
        // Skip devices that do not belong to the target seat.
        if dev_seat == seat
            // Resolve the device node (e.g. /dev/input/event3).
            && let Some(devnode) = udev_device.devnode()
            // Get evdev::Device
            && let Ok(device) = Device::open(devnode)
            // Skip pointing devices announced as keyboards
            && !device.supported_events().contains(EventType::ABSOLUTE)
        {
            return Ok(device);
        }
    }

    Err(format!("No keyboard device found for seat {seat}").into())
}

/// Fallback: scan `/dev/input/event*` and return the first keyboard-capable device.
fn find_keyboard_device_fallback() -> Result<Device, Box<dyn std::error::Error>>
{
    use std::{fs, path::Path};

    let input_path = Path::new("/dev/input");
    if !input_path.exists() {
        return Err("No /dev/input directory found.".into());
    }

    for entry in fs::read_dir(input_path)? {
        let path = entry?.path();
        if path.to_string_lossy().starts_with("/dev/input/event")
            && let Ok(device) = Device::open(&path)
            && device
                .supported_keys()
                .is_some_and(|keys| keys.contains(KeyCode::KEY_ENTER))
        {
            return Ok(device);
        }
    }

    Err("No keyboard device found. Try: sudo usermod -aG input \
                    $USER"
        .into())
}

pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut raw_device = find_keyboard_device()?;
    raw_device.grab()?;

    let mut virtual_device = uinput::default()?
        .name("CrossPlatform_Virtual_Keyboard")?
        .event(uinput::event::Keyboard::All)?
        .create()?;

    thread::sleep(Duration::from_millis(200));
    println!("Linux uinput virtual keyboard ready.");

    let shutdown = Arc::new(AtomicBool::new(false));
    register(SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    register(SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");

    let mut active_modifiers: u8 = 0;

    while !shutdown.load(Ordering::Acquire) {
        match raw_device.fetch_events() {
            Ok(events) => {
                for event in events {
                    if event.event_type() == EventType::KEY {
                        let code = event.code();
                        let value = event.value();

                        // Capture the modifier state to use for rule matching.
                        // For modifier keys this is the pre-update snapshot so
                        // that bare-modifier triggers (e.g. "LeftControl: A")
                        // match correctly against the concurrent modifier set.
                        let lookup_modifiers = active_modifiers;

                        if let Some(bit) = keycode_to_modifier_bit(code) {
                            if value == 1 {
                                active_modifiers |= 1 << bit;
                            } else if value == 0 {
                                active_modifiers &= !(1 << bit);
                            }
                        }

                        let guard = lookup.read();
                        let active_outputs = guard
                            .for_app(
                                guard.active_app(),
                                code,
                                lookup_modifiers,
                            )
                            .or_else(|| guard.global(code, lookup_modifiers))
                            .map(|v| v.to_vec());
                        drop(guard);

                        if let Some(outputs) = active_outputs {
                            // Emit mapped outputs and swallow the original
                            // event.  This applies to modifier keys as well:
                            // if a bare modifier (e.g. LeftControl alone) is
                            // mapped, its outputs are emitted and the original
                            // modifier press is NOT forwarded to the virtual
                            // device, preventing double emission.
                            if value == 1 {
                                for native_key in &outputs {
                                    if let Err(e) = emit_key_event(
                                        &mut virtual_device,
                                        native_key,
                                    ) {
                                        eprintln!("emit error: {}", e);
                                    }
                                }
                            }
                            continue;
                        }

                        if value == 1 {
                            virtual_device.write(
                                EventType::KEY.0 as _,
                                code as _,
                                1,
                            )?;
                        } else if value == 0 {
                            virtual_device.write(
                                EventType::KEY.0 as _,
                                code as _,
                                0,
                            )?;
                        } else {
                            virtual_device.write(
                                EventType::KEY.0 as _,
                                code as _,
                                1,
                            )?;
                            virtual_device.write(
                                EventType::KEY.0 as _,
                                code as _,
                                0,
                            )?;
                        }
                        virtual_device.synchronize()?;
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                eprintln!("Linux: error reading events: {}", e);
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    println!("Shutdown signal received. Cleaning up...");
    Ok(())
}
