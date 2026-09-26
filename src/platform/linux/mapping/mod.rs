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

use device::{
    ManagedDevice, capture_held_keys, drain_pending_events, emit_actions,
    native_release_window, plan_initial_state, process_device_events,
    sync_initial_state,
};
use epoll::{EpollFd, epoll_add, epoll_wait_raw};
use evdev::{AttributeSet, Device, KeyCode, uinput::VirtualDevice};
use hotplug::start_hotplug_monitor;
use libc::epoll_event;
use log::{error, info, warn};
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
    daemon::{engine::MappingEngine, state::Lookup},
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

pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    keyboard_filter: Option<Vec<KeyboardSpecifier>>,
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
        info!("No keyboards to grab. Waiting for events...");
    }

    // Grab and register all opened keyboards.
    let mut managed_devices: Vec<ManagedDevice> = Vec::new();
    for (kb, mut device) in opened_to_grab {
        device.grab()?;
        device.set_nonblocking(true)?;
        // Grabbing does not flush the kernel's event ring: drop everything
        // already buffered (e.g. the Enter press that started the daemon)
        // so the stream starts clean at the grab.
        let drained = drain_pending_events(&mut device, &kb.device);

        info!("Grabbed keyboard: {} ({})", kb.device, kb.name);
        let mut managed = ManagedDevice {
            device,
            path: kb.device,
            engine: MappingEngine::new(Arc::clone(&lookup)),
            pending_scan: None,
            // Synced inline below, before the event loop starts.
            pending_initial_state: false,
            pending_held_modifiers: Vec::new(),
        };
        // Capture the held keys into the engine right now, while the
        // kernel's key state still reflects the grab instant: a key
        // released before a late sample would leave its auto-repeat tail in
        // the ring, and repeats for a key the engine never saw down for are
        // decided as a fresh press (each forwarded as a tap).  The held
        // modifiers' key-down re-emission waits for the virtual device.
        let held_at_grab = capture_held_keys(&mut managed);
        // Let grab-time releases flow natively to the compositor (see
        // native_release_window) before the virtual device is built, so the
        // compositor's per-device key state stays in sync and the first
        // presses after startup are not swallowed.
        native_release_window(&mut managed, held_at_grab, drained > 0);
        managed_devices.push(managed);
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
    info!("Linux virtual keyboard ready.");

    // Re-emit the key-downs of the modifiers that were held when their
    // devices were grabbed (see capture_held_keys), now that the virtual
    // device exists.  This runs before the event loop so the first real
    // event is never processed against a stale (neutral) modifier state —
    // the root cause of a held-at-start modifier breaking later key
    // combinations.
    for managed in &mut managed_devices {
        sync_initial_state(managed, &mut virtual_device);
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    register(SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    register(SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");

    // Set up epoll for multiplexing across all managed devices.  `EpollFd`
    // owns the epoll fd and closes it on drop.
    let epoll_fd = EpollFd::new().map_err(|e| {
        error!("Linux: failed to create epoll instance: {e}");
        e
    })?;

    for managed in &managed_devices {
        let fd = managed.device.as_raw_fd();
        epoll_add(epoll_fd.as_raw_fd(), fd, fd as u64)?;
    }

    // Share the managed devices vector with the hot-plug monitor.  The main
    // event loop locks it only for the brief decision phase; the paced
    // emissions run outside the lock (SEC-14).
    let managed_devices = Arc::new(Mutex::new(managed_devices));

    // Start hot-plug monitor for dynamic device add/remove.  A thread
    // spawn failure (e.g. fd or thread exhaustion) is non-fatal, matching
    // the control socket: the daemon keeps serving the devices grabbed at
    // startup, only dynamic add/remove is lost until a restart.
    if let Err(e) = start_hotplug_monitor(
        Arc::clone(&lookup),
        Arc::clone(&managed_devices),
        epoll_fd.as_raw_fd(),
        keyboard_filter,
        Arc::clone(&shutdown),
    ) {
        warn!(
            "Failed to start hot-plug monitor ({e}); hot-plug handling is \
             disabled until the daemon is restarted."
        );
    }

    let mut events = vec![epoll_event { events: 0, u64: 0 }; 64];

    while !shutdown.load(Ordering::Acquire) {
        match epoll_wait_raw(epoll_fd.as_raw_fd(), &mut events) {
            Ok(n) => {
                for event in &events[..n as usize] {
                    let fd = event.u64 as RawFd;

                    // Short critical section: find the managed device for
                    // this fd and run its events through the engine, which
                    // returns the output actions to perform.  Nothing here
                    // sleeps, so the hot-plug thread is never blocked for more
                    // than the decision time.  The emissions pace their
                    // sub-events with ~20 ms sleeps, so they run below, once
                    // the lock is released (SEC-14).
                    let actions = {
                        let mut devices = managed_devices.lock();
                        match devices
                            .iter_mut()
                            .find(|m| m.device.as_raw_fd() == fd)
                        {
                            Some(managed) => {
                                let mut actions = Vec::new();
                                // A hot-plugged device was adopted before its
                                // key state was synced; sync it now, on the
                                // first event from it, so that event is not
                                // processed against a stale state.
                                if managed.pending_initial_state {
                                    actions
                                        .extend(plan_initial_state(managed));
                                    managed.pending_initial_state = false;
                                }
                                actions.extend(process_device_events(managed));
                                actions
                            }
                            None => continue,
                        }
                    };

                    emit_actions(&mut virtual_device, &actions);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                // Signal interruption — normal, just loop again.
                continue;
            }
            Err(e) => {
                error!("Linux: epoll wait error: {e}");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    info!("Shutdown signal received. Cleaning up...");
    Ok(())
}
