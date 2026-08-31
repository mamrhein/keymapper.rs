// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux direct-capture backend for the keyboard monitor.
//!
//! Instead of a GUI window — whose focus is controlled by the window
//! manager and can be stolen at any time — the Linux monitor grabs the
//! daemon's own uinput output device and logs the raw key events it
//! emits.  This makes the capture deterministic and headless-friendly,
//! and guarantees the daemon's output never leaks into the compositor or
//! any focused window (the e2e tests run on interactive sessions, where
//! a monitor window would otherwise be typed into by the user's own
//! keystrokes or, worse, steal focus from the user's editor).

use std::{
    fs, mem,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

use evdev::{Device, EventType};

use super::{OutputEvent, register_signal_handlers, writer::EventWriter};
use crate::platform::{
    VIRTUAL_KEYBOARD_NAME, hid_translate::keycode_to_hid_usage,
};

/// Interval between polls while waiting for the daemon's virtual device.
const DEVICE_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long to wait for the daemon's virtual device to appear.
const DEVICE_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
/// Sleep between non-blocking reads when no events are pending.
const READ_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Find the `/dev/input/event*` node of the daemon's virtual output device
/// by scanning `/sys/class/input` for the device name.
fn find_virtual_device() -> Option<PathBuf> {
    let Ok(entries) = fs::read_dir("/sys/class/input") else {
        return None;
    };

    for entry in entries.filter_map(Result::ok) {
        let file_name = entry.file_name();
        let name_path = entry.path().join("device/name");
        if let Ok(name) = fs::read_to_string(&name_path)
            && name.trim() == VIRTUAL_KEYBOARD_NAME
        {
            return Some(Path::new("/dev/input").join(file_name));
        }
    }

    None
}

/// Wait until the daemon's virtual device appears, then open and grab it.
///
/// The daemon never grabs its own output device, so the grab only fails if
/// a stale monitor process from a previous run is still attached.  In that
/// case the grab is retried on the next poll.
fn wait_for_and_grab_device() -> Device {
    let deadline = Instant::now() + DEVICE_WAIT_TIMEOUT;

    loop {
        if let Some(path) = find_virtual_device()
            && let Ok(mut device) = Device::open(&path)
            && device.set_nonblocking(true).is_ok()
            && device.grab().is_ok()
        {
            eprintln!("monitor: grabbing {path:?}");
            return device;
        }
        // Grab failed (e.g. EBUSY from a stale monitor) — retry.
        if Instant::now() >= deadline {
            panic!(
                "the daemon's virtual device ({VIRTUAL_KEYBOARD_NAME}) did \
                 not appear within {} ms; did the daemon fail to start?",
                DEVICE_WAIT_TIMEOUT.as_millis()
            );
        }

        thread::sleep(DEVICE_POLL_INTERVAL);
    }
}

/// Entry point for the Linux direct-capture monitor.
///
/// Waits for the daemon's virtual output device, grabs it, and logs every
/// key event to the output file until SIGTERM/SIGINT, or until the daemon
/// destroys its device on shutdown (whichever comes first).
pub fn run(output_path: &Path) {
    let mut writer = EventWriter::new(output_path)
        .expect("failed to open output file for event logging");
    let mut device = wait_for_and_grab_device();
    let shutdown = register_signal_handlers();

    // Set when the loop exits because the daemon destroyed its device.
    let mut device_removed = false;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        match device.fetch_events() {
            Ok(events) => {
                for event in events {
                    if event.event_type() != EventType::KEY {
                        continue;
                    }

                    let value = event.value();
                    if value == 2 {
                        // Auto-repeat: the daemon never emits repeats, and
                        // the test sequences never hold a key long enough
                        // to generate them.  Ignore.
                        continue;
                    }

                    let Some(key) = keycode_to_hid_usage(event.code()) else {
                        continue;
                    };

                    let _ = writer.write(OutputEvent {
                        down: value == 1,
                        key,
                    });
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(READ_POLL_INTERVAL);
            }
            Err(e) => {
                if e.raw_os_error() == Some(libc::ENODEV) {
                    // The daemon destroyed its uinput device on
                    // shutdown, and the kernel rejects further reads on
                    // the vanished device with ENODEV.  This is the
                    // expected end of the capture, not a fault.
                    eprintln!("monitor: device removed, exiting");
                    device_removed = true;
                } else {
                    eprintln!("monitor: read error: {e}");
                }
                break;
            }
        }
    }

    if device_removed {
        // evdev's Drop glue unconditionally ungrabs the device, which
        // fails on the destroyed device and prints a spurious error.
        // Leak the fd instead; the kernel reclaims it when this
        // short-lived process exits.
        mem::forget(device);
    }
}
