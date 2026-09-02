// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux keyboard capture and virtual output.
//!
//! Grabs the evdev devices matching the global keyboard filter, multiplexes
//! their input with epoll, and re-emits every key through a uinput virtual
//! keyboard: mapped keys as their mapped output, unmapped keys forwarded
//! unchanged. A background udev monitor adopts hot-plugged keyboards and
//! releases removed ones.
//!
//! The heavy lifting is split across three submodules: `epoll` wraps the raw
//! epoll FFI, `device` holds the per-device state and event processing, and
//! `hotplug` runs the udev add/remove monitor.

mod device;
mod epoll;
mod hotplug;

use std::{
    os::unix::io::{AsRawFd, RawFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use device::{ManagedDevice, process_device_events};
use epoll::{EpollFd, epoll_add, epoll_wait_raw};
use evdev::{AttributeSet, Device, KeyCode, uinput::VirtualDevice};
use hotplug::start_hotplug_monitor;
use libc::epoll_event;
use parking_lot::{Mutex, RwLock};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag::register,
};

use super::keyboard::discover_and_open_keyboards;
use crate::{
    common::keyboard::{
        KeyboardInfo, KeyboardSpecifier, filter_keyboards_by_specifiers,
    },
    daemon::state::Lookup,
};

/// Name of the daemon's own uinput output device.
///
/// Exposed so the e2e monitor (Linux direct-capture mode) can locate and
/// grab the device; the daemon itself never grabs it (see `handle_device_add`
/// in the `hotplug` submodule).
pub const VIRTUAL_KEYBOARD_NAME: &str = "CrossPlatform_Virtual_Keyboard";

// ---------------------------------------------------------------------------
// evdev event loop (epoll-based, multi-device)
// ---------------------------------------------------------------------------

/// `ready_signal` is invoked once the daemon can process events; it is
/// injected by the caller so this module stays free of test-specific side
/// effects.
pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    keyboard_filter: Option<Vec<KeyboardSpecifier>>,
    ready_signal: Option<Box<dyn FnOnce() + Send>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Discover and open keyboards for capture.  Degrade gracefully: with no
    // keyboards the daemon starts with an empty managed set, and the hot-plug
    // monitor picks devices up as they appear.
    let opened = discover_and_open_keyboards().unwrap_or_default();

    // Select the devices matching the keyboard filter, then filter the opened
    // pairs down to that grab set.
    let infos: Vec<KeyboardInfo> =
        opened.iter().map(|(info, _)| info.clone()).collect();
    let to_grab: Vec<KeyboardInfo> =
        filter_keyboards_by_specifiers(&infos, keyboard_filter.as_deref());
    let grab_paths: std::collections::HashSet<&str> =
        to_grab.iter().map(|kb| kb.device.as_str()).collect();
    let opened_to_grab: Vec<(KeyboardInfo, Device)> = opened
        .into_iter()
        .filter(|(info, _)| grab_paths.contains(info.device.as_str()))
        .collect();

    if opened_to_grab.is_empty() {
        println!("No keyboards to grab. Waiting for events...");
    }

    // Grab and register all opened keyboards.
    let mut managed_devices: Vec<ManagedDevice> = Vec::new();
    for (kb, mut device) in opened_to_grab {
        device.grab()?;
        device.set_nonblocking(true)?;

        println!("Grabbed keyboard: {} ({})", kb.device, kb.name);
        managed_devices.push(ManagedDevice {
            device,
            path: kb.device,
            modifiers: 0,
            forwarded_modifiers: 0,
            consumed_modifiers: 0,
            pending_scan: None,
        });
    }

    // KEY_CNT is the total number of key codes defined by the kernel
    // (linux/input.h: #define KEY_CNT (KEY_MAX + 1), where KEY_MAX = 0x2fd).
    const KEY_CNT: u16 = 0x2fe;
    let all_keys: AttributeSet<KeyCode> =
        (0..KEY_CNT).map(KeyCode::new).collect();
    let mut virtual_device = VirtualDevice::builder()?
        .name(VIRTUAL_KEYBOARD_NAME)
        .with_keys(&all_keys)?
        .build()?;

    thread::sleep(Duration::from_millis(200));
    println!("Linux virtual keyboard ready.");

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
        keyboard_filter,
    );

    // All devices are grabbed, the virtual output device is created, and the
    // hot-plug monitor is running, so the daemon can now process events.
    if let Some(signal) = ready_signal {
        signal();
    }

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
