// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    os::unix::io::AsRawFd,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use evdev::{Device, EventType};
use nix::sys::epoll::{
    Epoll, EpollCreateFlags, EpollEvent, EpollFlags, EpollTimeout,
};
use parking_lot::RwLock;
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag::register,
};

use super::key::Key;
use crate::{
    common::{keyboard::KeyboardInfo, modifier::ModifierRole},
    daemon::{mapping_cache::NativeKey, state::Lookup},
};

// ---------------------------------------------------------------------------
// Per-device state
// ---------------------------------------------------------------------------

/// A single managed keyboard device, tracking its own modifier state.
struct ManagedDevice {
    device: Device,
    /// Device node path (e.g. `/dev/input/event3`), used for rule lookup.
    path: String,
    /// Bitmask of currently active modifiers for this device only.
    modifiers: u8,
}

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
// Per-device event processing
// ---------------------------------------------------------------------------

/// Process all pending events for a single managed device.
///
/// Uses the device's own modifier state and path for rule lookup, ensuring
/// that modifier state on one keyboard does not affect another.
fn process_device_events(
    managed: &mut ManagedDevice,
    virtual_device: &mut uinput::Device,
    lookup: &Arc<RwLock<dyn Lookup>>,
) {
    // Drain all pending events from this non-blocking device.
    let events = match managed.device.fetch_events() {
        Ok(events) => events,
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            return;
        }
        Err(e) => {
            eprintln!(
                "Linux: error reading events from {}: {}",
                managed.path, e
            );
            return;
        }
    };

    for event in events {
        if event.event_type() != EventType::KEY {
            continue;
        }

        let code = event.code();
        let value = event.value();

        // Capture the modifier state to use for rule matching.  For modifier
        // keys this is the pre-update snapshot so that bare-modifier triggers
        // (e.g. "LeftControl: A") match correctly against the concurrent
        // modifier set.
        let lookup_modifiers = managed.modifiers;

        if let Some(bit) = keycode_to_modifier_bit(code) {
            if value == 1 {
                managed.modifiers |= 1 << bit;
            } else if value == 0 {
                managed.modifiers &= !(1 << bit);
            }
        }

        let device_path = &managed.path;

        let guard = lookup.read();
        let active_outputs = guard
            .for_app(
                &guard.active_app(),
                code,
                lookup_modifiers,
                Some(device_path),
            )
            .or_else(|| {
                guard.global(code, lookup_modifiers, Some(device_path))
            })
            .map(|v| v.to_vec());
        drop(guard);

        if let Some(outputs) = active_outputs {
            // Emit mapped outputs and swallow the original event.  This
            // applies to modifier keys as well: if a bare modifier
            // (e.g. LeftControl alone) is mapped, its outputs are emitted
            // and the original modifier press is NOT forwarded to the
            // virtual device, preventing double emission.
            if value == 1 {
                for native_key in &outputs {
                    if let Err(e) = emit_key_event(virtual_device, native_key)
                    {
                        eprintln!("emit error: {}", e);
                    }
                }
            }
            continue;
        }

        // Forward the event to the virtual device.
        if value == 1 {
            if let Err(e) =
                virtual_device.write(EventType::KEY.0 as _, code as _, 1)
            {
                eprintln!("write error: {}", e);
            }
        } else if value == 0 {
            if let Err(e) =
                virtual_device.write(EventType::KEY.0 as _, code as _, 0)
            {
                eprintln!("write error: {}", e);
            }
        } else {
            // Repeat event (value == 2): emit as press+release to avoid
            // key-stick on the virtual device.
            if let Err(e) =
                virtual_device.write(EventType::KEY.0 as _, code as _, 1)
            {
                eprintln!("write error: {}", e);
            }
            if let Err(e) =
                virtual_device.write(EventType::KEY.0 as _, code as _, 0)
            {
                eprintln!("write error: {}", e);
            }
        }
        if let Err(e) = virtual_device.synchronize() {
            eprintln!("sync error: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// evdev event loop (epoll-based, multi-device)
// ---------------------------------------------------------------------------

/// Open a keyboard device, grab it, and prepare it for epoll monitoring.
fn open_and_grab_device(
    kb: &KeyboardInfo,
) -> Result<ManagedDevice, Box<dyn std::error::Error>> {
    let mut device = Device::open(&kb.device)?;
    device.grab()?;
    device.set_nonblocking(true)?;

    Ok(ManagedDevice {
        device,
        path: kb.device.clone(),
        modifiers: 0,
    })
}

pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    keyboards_to_grab: Vec<KeyboardInfo>,
) -> Result<(), Box<dyn std::error::Error>> {
    if keyboards_to_grab.is_empty() {
        println!("No keyboards to grab. Waiting for events...");
    }

    // Open, grab and register all keyboards.
    let mut managed_devices: Vec<ManagedDevice> = Vec::new();
    for kb in &keyboards_to_grab {
        match open_and_grab_device(kb) {
            Ok(managed) => {
                println!("Grabbed keyboard: {} ({})", managed.path, kb.name);
                managed_devices.push(managed);
            }
            Err(e) => {
                eprintln!("Warning: failed to open/grab {}: {}", kb.device, e);
            }
        }
    }

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

    // Set up epoll for multiplexing across all managed devices.
    let epoll = Epoll::new(EpollCreateFlags::empty())?;

    for managed in &managed_devices {
        let fd = managed.device.as_raw_fd();
        epoll.add(
            &managed.device,
            EpollEvent::new(
                EpollFlags::EPOLLIN | EpollFlags::EPOLLET,
                fd as u64,
            ),
        )?;
    }

    let mut events = vec![EpollEvent::empty(); 64];

    while !shutdown.load(Ordering::Acquire) {
        match epoll.wait(&mut events, EpollTimeout::NONE) {
            Ok(n) => {
                for event in &events[..n] {
                    let fd = event.data() as i32;

                    // Find the managed device for this file descriptor and
                    // process its events.
                    if let Some(managed) = managed_devices
                        .iter_mut()
                        .find(|m| m.device.as_raw_fd() == fd)
                    {
                        process_device_events(
                            managed,
                            &mut virtual_device,
                            &lookup,
                        );
                    }
                }
            }
            Err(nix::errno::Errno::EINTR) => {
                // Signal interruption — normal, just loop again.
                continue;
            }
            Err(e) => {
                eprintln!("Linux: epoll wait error: {}", e);
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    println!("Shutdown signal received. Cleaning up...");
    Ok(())
}
