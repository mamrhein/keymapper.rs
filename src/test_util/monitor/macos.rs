// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS IOKit-seizure backend for the keyboard monitor.
//!
//! Instead of a focused GUI window — whose capture depends on the window
//! server keeping keyboard focus on it, and which is therefore brittle and
//! steals focus from the user on an interactive session — the macOS monitor
//! seizes the daemon's own Karabiner DriverKit virtual keyboard and logs the
//! raw HID events it emits.  This is the true analogue of the Linux uinput
//! grab: it is deterministic, needs no window or keyboard focus, and is
//! headless friendly.
//!
//! Seizing alone does not stop the daemon's output from leaking into the
//! compositor or any focused window: unlike for physical keyboards, IOKit
//! seizure is not exclusive for DriverKit virtual keyboards, so their events
//! are delivered to both the seizing process and the WindowServer.  The
//! monitor therefore also installs a CGEventTap that consumes every keyboard
//! event before it can reach an application.

use std::{
    collections::HashSet,
    ffi::c_void,
    path::Path,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes,
    kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGEvent, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};

use super::{OutputEvent, register_signal_handlers, writer::EventWriter};
use crate::{
    common::hid_usage::HidUsage,
    platform::{HidDevice, HidDeviceManager, IOHIDQueue, for_each_hid_value},
};

/// How long to wait for the virtual keyboard to appear.
const VIRTUAL_KEYBOARD_WAIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Context passed to the monitor's queue value callback.
struct MonitorContext {
    /// File writer for captured events.
    writer: EventWriter,
    /// Combined usage codes of keys currently down, for repeat suppression.
    pressed: HashSet<u32>,
}

/// FFI callback invoked by IOHIDQueue when values are available.
///
/// Matches the C `IOHIDCallback` signature: `(void *context, IOReturn
/// result, void *sender)`, where `sender` is the queue.  Values are drained
/// from the queue by `for_each_hid_value`.  Logs each keyboard/consumer key
/// as `down <Key>` / `up <Key>`, mapping the raw HID usage to a `HidUsage`
/// via its combined code.  Repeats (a key-down for a key already down, or a
/// key-up for a key not down) are suppressed so the log matches the daemon's
/// emission one-for-one.
unsafe extern "C" fn monitor_value_callback(
    user_info: *mut c_void,
    _result: i32,
    queue: *mut IOHIDQueue,
) {
    if user_info.is_null() || queue.is_null() {
        return;
    }

    let context = unsafe { &mut *(user_info as *mut MonitorContext) };

    unsafe {
        for_each_hid_value(queue, |usage_code, is_down| {
            let Some(key) = HidUsage::from_code(usage_code) else {
                return;
            };

            // Suppress repeats so the log matches the daemon's emission.
            if is_down {
                if !context.pressed.insert(usage_code) {
                    return;
                }
            } else if !context.pressed.remove(&usage_code) {
                return;
            }

            let _ = context.writer.write(OutputEvent { down: is_down, key });
        });
    }
}

/// Event-tap callback that consumes every keyboard event.
///
/// Returning null drops the event, so it never reaches the WindowServer's
/// application dispatch.  All keyboard events are consumed (not just those
/// from the virtual keyboards) because the daemon seizes the user's physical
/// keyboard, so no legitimate input can reach this level during a test run.
unsafe extern "C-unwind" fn suppress_callback(
    _proxy: CGEventTapProxy,
    _event_type: CGEventType,
    _event: core::ptr::NonNull<CGEvent>,
    _user_info: *mut c_void,
) -> *mut CGEvent {
    std::ptr::null_mut()
}

/// Install a CGEventTap that consumes all keyboard events, and return the tap
/// together with its run loop source.
///
/// The returned values must stay alive for the duration of the run: dropping
/// the tap invalidates it.  Returns `None` (with a diagnostic) if the tap
/// cannot be created, e.g. because Accessibility access is denied; the IOKit
/// capture still works in that case, only the leak suppression is lost.
fn install_suppression_tap()
-> Option<(CFRetained<CFMachPort>, CFRetained<CFRunLoopSource>)> {
    let mask: u64 = (1u64 << CGEventType::KeyDown.0)
        | (1u64 << CGEventType::KeyUp.0)
        | (1u64 << CGEventType::FlagsChanged.0);

    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(suppress_callback),
            std::ptr::null_mut(),
        )
    };

    let Some(tap) = tap else {
        eprintln!(
            "monitor: failed to create the suppression event tap; verify \
             Accessibility privileges (leaked events may reach the focused \
             application)"
        );
        return None;
    };

    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&tap), 0)
    else {
        eprintln!(
            "monitor: failed to create the run loop source for the \
             suppression tap (leaked events may reach the focused \
             application)"
        );
        return None;
    };

    CFRunLoop::current()
        .expect("no current run loop")
        .add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
    CGEvent::tap_enable(&tap, true);

    eprintln!("monitor: suppressing leaked keyboard events via CGEventTap");

    Some((tap, source))
}

/// Wait until the Karabiner virtual keyboard appears, then return it.
///
/// The virtual keyboard is created by the Karabiner DriverKit daemon and may
/// take a moment to load into IOKit on a fresh runner, so it is polled until
/// it appears or the timeout elapses.
///
/// The manager is scheduled with the current run loop and that run loop is
/// pumped while waiting: `IOHIDManagerCopyDevices` only reflects devices the
/// manager has been notified about, and those hotplug notifications are
/// delivered through the run loop.  The virtual keyboard appears *after* the
/// monitor's manager is created (the daemon starts later), so without pumping
/// the run loop the manager would never learn about it.
fn wait_for_virtual_keyboard(manager: &HidDeviceManager) -> HidDevice {
    let deadline = Instant::now() + VIRTUAL_KEYBOARD_WAIT_TIMEOUT;

    manager.schedule_with_runloop();

    let mut last_log = Instant::now();
    loop {
        if let Some(device) = manager.find_karabiner_virtual_keyboard() {
            return device;
        }

        if Instant::now() >= deadline {
            panic!(
                "the Karabiner virtual keyboard did not appear within {} ms; \
                 is the DriverKit driver loaded?",
                VIRTUAL_KEYBOARD_WAIT_TIMEOUT.as_millis()
            );
        }

        // Pump the run loop so hotplug notifications are processed; a short
        // timeout keeps the deadline check responsive.
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.1, true);

        // Log progress once per second so a stuck wait is diagnosable: the
        // count includes the virtual keyboard, so it should rise by one when
        // the device appears.
        if last_log.elapsed() >= Duration::from_secs(1) {
            last_log = Instant::now();
            eprintln!(
                "monitor: still waiting for the Karabiner virtual keyboard; \
                 {} device(s) currently matched",
                manager.matched_device_count()
            );
        }
    }
}

/// Entry point for the macOS IOKit-seizure monitor.
///
/// Seizes the daemon's Karabiner DriverKit virtual keyboard and logs every key
/// event it emits to the output file until SIGTERM/SIGINT.  A CGEventTap
/// additionally consumes every keyboard event, so the daemon's output never
/// leaks into the compositor or any focused window (see
/// `install_suppression_tap` for why seizure alone is not enough).
pub fn run(output_path: &Path) {
    let writer = EventWriter::new(output_path)
        .expect("failed to open output file for event logging");
    let shutdown = register_signal_handlers();

    // Discover keyboards and wait for the virtual keyboard to appear.
    let manager = HidDeviceManager::new_keyboard_matcher()
        .expect("failed to create IOHIDManager");
    let device = wait_for_virtual_keyboard(&manager);

    eprintln!(
        "monitor: seizing Karabiner virtual keyboard at {}",
        device.location_id_string()
    );

    // Seize the device for exclusive input capture.
    device
        .open(true)
        .expect("failed to seize the Karabiner virtual keyboard");

    // Create a queue and register the logging callback.
    let queue = device
        .create_queue()
        .expect("failed to create IOHIDQueue for the virtual keyboard");
    let context = MonitorContext {
        writer,
        pressed: HashSet::new(),
    };
    let _handle =
        queue.register_value_callback_generic(monitor_value_callback, context);

    // Schedule and open the queue so events are delivered.
    queue.schedule_with_runloop();
    queue
        .open()
        .expect("failed to open IOHIDQueue for the virtual keyboard");

    eprintln!("monitor: capturing daemon-emitted keys via IOKit seizure");

    // Consume leaked keyboard events; the tap and its run loop source must
    // stay alive until shutdown.
    let _suppression = install_suppression_tap();

    // Poll the run loop until a shutdown signal is received.  The queue and
    // tap callbacks fire on this run loop; the 0.5s timeout keeps the
    // shutdown check responsive.
    while !shutdown.load(Ordering::Relaxed) {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.5, true);
    }

    // Cleanup happens via Drop: the queue handle closes the queue and frees
    // the context.  The seized device is released when the process exits.
}
