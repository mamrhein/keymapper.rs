// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    os::unix::io::{AsRawFd, RawFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use evdev::{Device, EventType};
use libc::{c_int, epoll_event, epoll_wait as libc_epoll_wait};
use parking_lot::{Mutex, RwLock};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag::register,
};
use udev::MonitorBuilder;

use super::key::Key;
use crate::{
    common::{
        keyboard::{
            KeyboardInfo, KeyboardSpecifier, filter_keyboards_by_specifiers,
        },
        modifier::ModifierRole,
    },
    daemon::{mapping_cache::NativeKey, state::Lookup},
};

// ---------------------------------------------------------------------------
// Raw epoll FFI for cross-thread operations
// ---------------------------------------------------------------------------
//
// The nix `Epoll` type owns the epoll fd but is not `Sync`, preventing
// safe sharing across threads.  Because epoll_ctl(2) and epoll_wait(2) are
// safe to call concurrently on the same epoll fd, we use thin libc wrappers
// for all epoll operations.  This avoids the nix `Epoll` type entirely and
// gives us full control over cross-thread access.

unsafe extern "C" {
    fn epoll_create1(flags: c_int) -> c_int;
    fn epoll_ctl(
        epfd: c_int,
        op: c_int,
        fd: c_int,
        event: *mut epoll_event,
    ) -> c_int;
}

/// Raw epoll control operation constants.
const EPOLL_CTL_ADD: c_int = 1;
const EPOLL_CTL_DEL: c_int = 2;

/// EPOLLIN | EPOLLET flag combination.
const EPOLL_IN_ET: u32 = 0x001 | (1 << 31);

/// Create a new epoll instance.  Returns the file descriptor on success.
fn epoll_create() -> Result<c_int, std::io::Error> {
    let fd = unsafe { epoll_create1(0) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(fd)
    }
}

/// Register a file descriptor with the epoll instance.
fn epoll_add(epfd: c_int, fd: c_int, data: u64) -> Result<(), std::io::Error> {
    let mut event = epoll_event {
        events: EPOLL_IN_ET,
        u64: data,
    };
    let ret = unsafe { epoll_ctl(epfd, EPOLL_CTL_ADD, fd, &mut event) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Remove a file descriptor from the epoll instance.
fn epoll_del(epfd: c_int, fd: c_int) -> Result<(), std::io::Error> {
    let ret =
        unsafe { epoll_ctl(epfd, EPOLL_CTL_DEL, fd, std::ptr::null_mut()) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Wait for events on the epoll instance.  Blocks indefinitely.
fn epoll_wait_raw(
    epfd: c_int,
    events: &mut [epoll_event],
) -> Result<c_int, std::io::Error> {
    let ret = unsafe {
        libc_epoll_wait(
            epfd,
            events.as_mut_ptr(),
            events.len() as c_int,
            -1, // Block indefinitely.
        )
    };
    if ret >= 0 {
        Ok(ret)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Manages the lifecycle of an epoll file descriptor.
///
/// Wraps a raw fd in a type-safe wrapper that closes the fd on drop.
struct EpollFd(c_int);

impl EpollFd {
    fn new() -> Result<Self, std::io::Error> {
        epoll_create().map(EpollFd)
    }

    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Drop for EpollFd {
    fn drop(&mut self) {
        let _ = unsafe { libc::close(self.0) };
    }
}

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
// Hot-plug monitor (udev-based, filter-aware)
// ---------------------------------------------------------------------------

/// Spawn a background thread that listens for keyboard device add/remove
/// events via udev and dynamically updates the managed device set.
///
/// New devices are only grabbed if they match the global keyboard filter.
/// Removed devices are ungrabbed and removed from the epoll set.
///
/// **Limitation:** changes to the global `keyboards:` filter at runtime do
/// not affect the grab list. The user must restart the daemon for
/// filter changes to take effect on hot-plugged devices.
fn start_hotplug_monitor(
    managed_devices: Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
    global_filter: Option<Vec<KeyboardSpecifier>>,
) {
    use udev::EventType;

    thread::Builder::new()
        .name("keymapper-hotplug".into())
        .spawn(move || {
            // Set up the udev monitor.
            let socket = match MonitorBuilder::new() {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("warning: failed to create udev monitor: {e}");
                    return;
                }
            };

            let socket = match socket.match_subsystem("input") {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("warning: failed to match input subsystem: {e}");
                    return;
                }
            };

            let socket = match socket.listen() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("warning: failed to start udev monitor: {e}");
                    return;
                }
            };

            println!("Hot-plug monitor started.");

            for event in socket.iter() {
                let udev_device = event.device();

                // Filter for keyboards manually, since the netlink monitor
                // doesn't support property-based filtering.
                let is_keyboard = udev_device
                    .property_value("ID_INPUT_KEYBOARD")
                    .map(|s| s.to_string_lossy() == "1")
                    .unwrap_or(false);
                if !is_keyboard {
                    continue;
                }

                match event.event_type() {
                    EventType::Add => {
                        handle_device_add(
                            &udev_device,
                            &managed_devices,
                            epoll_fd,
                            &global_filter,
                        );
                    }
                    EventType::Remove => {
                        handle_device_remove(
                            &udev_device,
                            &managed_devices,
                            epoll_fd,
                        );
                    }
                    _ => {}
                }
            }
        })
        .expect("failed to spawn hot-plug monitor thread");
}

/// Handle a udev "add" event for a keyboard device.
///
/// Opens the device, checks the global filter, grabs it, and registers it
/// with epoll and the managed device list.
fn handle_device_add(
    udev_device: &udev::Device,
    managed_devices: &Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
    global_filter: &Option<Vec<KeyboardSpecifier>>,
) {
    // Build keyboard info and open the evdev device.
    let Some((kb, mut device)) =
        super::keyboard::build_keyboard_from_udev(udev_device)
    else {
        return;
    };

    // Check if it matches the global filter.
    let filtered = filter_keyboards_by_specifiers(
        std::slice::from_ref(&kb),
        global_filter.as_deref(),
    );
    if filtered.is_empty() {
        println!(
            "Hot-plug: ignoring {} (does not match global filter)",
            kb.name
        );
        return;
    }

    // Skip if this device is already managed.
    {
        let devices = managed_devices.lock();
        if devices.iter().any(|m| m.path == kb.device) {
            return;
        }
    }

    // Grab and configure the device.
    if let Err(e) = device.grab() {
        eprintln!("warning: failed to grab {}: {e}", kb.device);
        return;
    }

    if let Err(e) = device.set_nonblocking(true) {
        eprintln!("warning: failed to set non-blocking on {}: {e}", kb.device);
        return;
    }

    let fd = device.as_raw_fd();
    let managed = ManagedDevice {
        device,
        path: kb.device.clone(),
        modifiers: 0,
    };

    // Register with managed devices.
    {
        let mut devices = managed_devices.lock();
        devices.push(managed);
    }

    // Register with epoll.
    if let Err(e) = epoll_add(epoll_fd, fd, fd as u64) {
        eprintln!("warning: failed to add {} to epoll: {e}", kb.device);
        // Rollback: remove from managed devices since epoll registration
        // failed.
        let mut devices = managed_devices.lock();
        if let Some(idx) = devices.iter().position(|m| m.path == kb.device) {
            devices.remove(idx);
        }
        return;
    }

    println!("Hot-plug: grabbed {} ({})", kb.device, kb.name);
}

/// Handle a udev "remove" event for a keyboard device.
///
/// Removes the device from epoll and the managed device list.
fn handle_device_remove(
    udev_device: &udev::Device,
    managed_devices: &Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
) {
    // Get the device path to identify the managed device.
    let dev_path = match udev_device.devnode() {
        Some(d) => d.to_string_lossy().into_owned(),
        None => {
            // Cannot identify the device without a devnode.
            eprintln!("warning: remove event without devnode, skipping");
            return;
        }
    };

    // Remove from managed devices and capture the fd for epoll cleanup.
    let fd = {
        let mut devices = managed_devices.lock();
        let idx = match devices.iter().position(|m| m.path == dev_path) {
            Some(i) => i,
            None => {
                // Not managed — nothing to do.
                return;
            }
        };

        let fd = devices[idx].device.as_raw_fd();
        devices.remove(idx); // Drops ManagedDevice, closing the fd.
        fd
    };

    // Delete from epoll.  The fd is now closed, but the kernel handles
    // this gracefully.  If the kernel already cleaned it up, this may
    // fail — log and ignore.
    if let Err(e) = epoll_del(epoll_fd as c_int, fd) {
        eprintln!("warning: failed to remove {dev_path} from epoll: {e}");
    }

    println!("Hot-plug: removed {dev_path}");
}

// ---------------------------------------------------------------------------
// evdev event loop (epoll-based, multi-device)
// ---------------------------------------------------------------------------

pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    keyboards_to_grab: Vec<(KeyboardInfo, Device)>,
    global_filter: Option<Vec<KeyboardSpecifier>>,
) -> Result<(), Box<dyn std::error::Error>> {
    if keyboards_to_grab.is_empty() {
        println!("No keyboards to grab. Waiting for events...");
    }

    // Grab and register all pre-opened keyboards.
    let mut managed_devices: Vec<ManagedDevice> = Vec::new();
    for (kb, mut device) in keyboards_to_grab {
        device.grab()?;
        device.set_nonblocking(true)?;

        println!("Grabbed keyboard: {} ({})", kb.device, kb.name);
        managed_devices.push(ManagedDevice {
            device,
            path: kb.device,
            modifiers: 0,
        });
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

    // Set up epoll for multiplexing across all managed devices.  `EpollFd`
    // owns the epoll fd and closes it on drop.
    let epoll_fd = EpollFd::new().map_err(|e| {
        eprintln!("Linux: failed to create epoll instance: {e}");
        e
    })?;

    for managed in &managed_devices {
        let fd = managed.device.as_raw_fd();
        epoll_add(epoll_fd.as_raw_fd(), fd, fd as u64)?;
    }

    // Share the managed devices vector with the hot-plug monitor.  The main
    // event loop locks it briefly while processing each device's events.
    let managed_devices = Arc::new(Mutex::new(managed_devices));

    // Start hot-plug monitor for dynamic device add/remove.
    start_hotplug_monitor(
        Arc::clone(&managed_devices),
        epoll_fd.as_raw_fd(),
        global_filter,
    );

    let mut events = vec![epoll_event { events: 0, u64: 0 }; 64];

    while !shutdown.load(Ordering::Acquire) {
        match epoll_wait_raw(epoll_fd.as_raw_fd(), &mut events) {
            Ok(n) => {
                for event in &events[..n as usize] {
                    let fd = event.u64 as RawFd;

                    // Find the managed device for this file descriptor and
                    // process its events.  The lock is held during processing
                    // to prevent the hot-plug thread from modifying the vec
                    // concurrently.  Hot-plug operations are rare, so the
                    // contention is negligible.
                    let mut devices = managed_devices.lock();
                    if let Some(managed) =
                        devices.iter_mut().find(|m| m.device.as_raw_fd() == fd)
                    {
                        process_device_events(
                            managed,
                            &mut virtual_device,
                            &lookup,
                        );
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                // Signal interruption — normal, just loop again.
                continue;
            }
            Err(e) => {
                eprintln!("Linux: epoll wait error: {e}");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    println!("Shutdown signal received. Cleaning up...");
    Ok(())
}
