// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Udev-based hot-plug monitor for dynamic device add/remove.
//!
//! A background thread listens for keyboard add/remove events via udev and
//! updates the managed device set, adopting new keyboards that match the
//! global filter and releasing removed ones. A one-time resync after
//! `listen()` closes the race window between the startup snapshot and the
//! monitor becoming active.

use std::{
    os::unix::io::{AsRawFd, RawFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use libc::c_int;
use log::{info, warn};
use parking_lot::{Mutex, RwLock};
use udev::{Enumerator, MonitorBuilder};

use super::{
    VIRTUAL_KEYBOARD_NAME,
    device::{
        ManagedDevice, capture_held_keys, drain_pending_events,
        native_release_window,
    },
    epoll::{epoll_add, epoll_del},
};
use crate::{
    common::keyboard::{KeyboardSpecifier, filter_keyboards_by_specifiers},
    daemon::{engine::MappingEngine, state::Lookup},
    platform::linux::keyboard::build_keyboard_from_udev,
};

/// Minimum delay before the supervisor restarts a monitor that exited
/// unexpectedly.
const RESTART_DELAY_MIN: Duration = Duration::from_secs(1);
/// Cap for the exponentially growing restart delay: a permanently broken
/// udev (or an exhausted fd budget) costs one retry per cap, not a spin.
const RESTART_DELAY_MAX: Duration = Duration::from_secs(30);
/// A monitor run at least this long is considered healthy, so the restart
/// delay resets to its minimum for the next failure.
const HEALTHY_RUN: Duration = Duration::from_secs(60);
/// Granularity of the interruptible sleep between monitor restarts.
const SHUTDOWN_CHECK_INTERVAL: Duration = Duration::from_millis(100);

/// Spawn a supervised background thread that listens for keyboard device
/// add/remove events via udev and dynamically updates the managed device
/// set.
///
/// New devices are only grabbed if they match the global keyboard filter.
/// Removed devices are ungrabbed and removed from the epoll set.
///
/// The monitor is supervised: when it exits unexpectedly (poll failure,
/// or the socket reporting `POLLERR`/`POLLHUP`/`POLLNVAL`), the thread
/// rebuilds it and keeps listening, with capped exponential backoff so a
/// permanently broken udev cannot spin.  Every (re)start re-syncs the
/// device set, so keyboards that appeared while the monitor was down are
/// adopted anyway.
///
/// Returns an error if the monitor thread cannot be spawned (e.g. thread
/// or fd exhaustion).  The caller treats that as non-fatal — matching the
/// control socket — and the daemon keeps serving the devices grabbed at
/// startup.
///
/// **Limitation:** changes to the global `keyboards:` filter at runtime do
/// not affect the grab list. The user must restart the daemon for
/// filter changes to take effect on hot-plugged devices.
pub(super) fn start_hotplug_monitor(
    lookup: Arc<RwLock<dyn Lookup>>,
    managed_devices: Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
    global_filter: Option<Vec<KeyboardSpecifier>>,
    shutdown: Arc<AtomicBool>,
) -> Result<(), std::io::Error> {
    thread::Builder::new()
        .name("keymapper-hotplug".into())
        .spawn(move || {
            supervise_hotplug(
                lookup,
                managed_devices,
                epoll_fd,
                global_filter,
                shutdown,
            );
        })
        .map(drop)
}

/// Keep [`run_hotplug_monitor`] running for the lifetime of the daemon.
///
/// Restarts the monitor when it exits unexpectedly, backing off
/// exponentially (capped) between restarts, and stops when `shutdown` is
/// set.
fn supervise_hotplug(
    lookup: Arc<RwLock<dyn Lookup>>,
    managed_devices: Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
    global_filter: Option<Vec<KeyboardSpecifier>>,
    shutdown: Arc<AtomicBool>,
) {
    let mut delay = RESTART_DELAY_MIN;
    while !shutdown.load(Ordering::Acquire) {
        let started = Instant::now();
        run_hotplug_monitor(
            &lookup,
            &managed_devices,
            epoll_fd,
            &global_filter,
        );
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        info!(
            "Hot-plug monitor exited; restarting it in {} s.",
            delay.as_secs()
        );
        sleep_or_shutdown(delay, &shutdown);
        delay = next_restart_delay(delay, started.elapsed() >= HEALTHY_RUN);
    }
}

/// Next restart delay for the monitor: reset to the minimum after a
/// healthy run, otherwise double the current delay, capped at
/// [`RESTART_DELAY_MAX`].
fn next_restart_delay(current: Duration, run_was_healthy: bool) -> Duration {
    if run_was_healthy {
        RESTART_DELAY_MIN
    } else {
        (current * 2).min(RESTART_DELAY_MAX)
    }
}

/// Sleep up to *delay*, returning early once *shutdown* is set.
fn sleep_or_shutdown(delay: Duration, shutdown: &AtomicBool) {
    let deadline = Instant::now() + delay;
    while !shutdown.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::sleep(SHUTDOWN_CHECK_INTERVAL);
    }
}

/// Run the udev monitor event loop until it becomes unusable.
///
/// Returns when the monitor cannot be (re)created, when `poll` fails, or
/// when the monitor socket reports `POLLERR`/`POLLHUP`/`POLLNVAL` (e.g.
/// after a udevd restart or under fd exhaustion).  [`supervise_hotplug`]
/// restarts this function when it returns.
fn run_hotplug_monitor(
    lookup: &Arc<RwLock<dyn Lookup>>,
    managed_devices: &Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
    global_filter: &Option<Vec<KeyboardSpecifier>>,
) {
    use udev::EventType;

    // Set up the udev monitor.
    let socket = match MonitorBuilder::new() {
        Ok(b) => b,
        Err(e) => {
            warn!("Failed to create udev monitor: {e}");
            return;
        }
    };

    let socket = match socket.match_subsystem("input") {
        Ok(b) => b,
        Err(e) => {
            warn!("Failed to match input subsystem: {e}");
            return;
        }
    };

    let socket = match socket.listen() {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to start udev monitor: {e}");
            return;
        }
    };

    info!("Hot-plug monitor started.");

    // Resync: the startup udev snapshot in `start_mapping` and this
    // monitor's `listen()` call are not atomic.  A keyboard added in
    // between (e.g. a test injector created moments before daemon
    // start, or a device whose udev tagging finished after the
    // snapshot) never emits a fresh "add" event to this monitor and
    // would otherwise never be grabbed.  Rescan and adopt any
    // missing keyboards to close that window.  The same holds across a
    // monitor restart: devices that appeared while the previous monitor
    // socket was dead emitted no event this socket will ever see.
    resync_devices(lookup, managed_devices, epoll_fd, global_filter);

    // The monitor socket is non-blocking:
    // `udev_monitor_receive_device` (what `socket.iter()`
    // calls) returns NULL as soon as no event is pending,
    // so a bare iteration loop would end immediately and
    // the monitor would die on startup.  Block in `poll` until udev
    // has an event, then receive it.
    let mut pollfd = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let mut iter = socket.iter();

    loop {
        let ret = unsafe { libc::poll(&mut pollfd, 1, -1) };
        if ret < 0 {
            if std::io::Error::last_os_error().raw_os_error()
                == Some(libc::EINTR)
            {
                continue;
            }
            warn!(
                "Udev monitor poll failed: {}",
                std::io::Error::last_os_error()
            );
            break;
        }
        if pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)
            != 0
        {
            warn!("Udev monitor socket closed.");
            break;
        }

        // `poll` reported data, but the receive can still find
        // nothing (spurious wake-up) — keep polling in that case.
        let Some(event) = iter.next() else {
            continue;
        };

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
                    lookup,
                    &udev_device,
                    managed_devices,
                    epoll_fd,
                    global_filter,
                );
            }
            EventType::Remove => {
                handle_device_remove(&udev_device, managed_devices, epoll_fd);
            }
            _ => {}
        }
    }
}

/// Rescan udev for keyboards that are not yet managed and adopt them.
///
/// Called once after the hot-plug monitor starts listening, to cover the
/// race window between the startup snapshot and the monitor's `listen()`
/// call.  Reuses [`handle_device_add`], which skips devices that are
/// already managed.
fn resync_devices(
    lookup: &Arc<RwLock<dyn Lookup>>,
    managed_devices: &Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
    global_filter: &Option<Vec<KeyboardSpecifier>>,
) {
    let Ok(mut enumerator) = Enumerator::new() else {
        warn!("Resync: failed to create udev enumerator");
        return;
    };

    if enumerator.match_subsystem("input").is_err()
        || enumerator.match_property("ID_INPUT_KEYBOARD", "1").is_err()
    {
        warn!("Resync: failed to configure udev enumerator");
        return;
    }

    let Ok(devices) = enumerator.scan_devices() else {
        warn!("Resync: failed to scan udev devices");
        return;
    };

    for udev_device in devices {
        handle_device_add(
            lookup,
            &udev_device,
            managed_devices,
            epoll_fd,
            global_filter,
        );
    }
}

/// Handle a udev "add" event for a keyboard device.
///
/// Opens the device, checks the global filter, grabs it, and registers it
/// with epoll and the managed device list.
fn handle_device_add(
    lookup: &Arc<RwLock<dyn Lookup>>,
    udev_device: &udev::Device,
    managed_devices: &Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
    global_filter: &Option<Vec<KeyboardSpecifier>>,
) {
    // Build keyboard info and open the evdev device.
    let Some((kb, mut device)) = build_keyboard_from_udev(udev_device) else {
        return;
    };

    // Skip the daemon's own virtual output device, which udev also tags as
    // a keyboard.  Grabbing it would feed the daemon's emitted events back
    // into its input loop, causing them to be re-emitted indefinitely.
    if kb.name == VIRTUAL_KEYBOARD_NAME {
        return;
    }

    // Check if it matches the global filter.
    let filtered = filter_keyboards_by_specifiers(
        std::slice::from_ref(&kb),
        global_filter.as_deref(),
    );
    if filtered.is_empty() {
        info!(
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
        warn!("Failed to grab {}: {e}", kb.device);
        return;
    }

    if let Err(e) = device.set_nonblocking(true) {
        warn!("Failed to set non-blocking on {}: {e}", kb.device);
        return;
    }

    // Same as at startup: grabbing does not flush the kernel's event ring,
    // so drop anything buffered before the grab to keep the adopted
    // device's stream clean.
    let drained = drain_pending_events(&mut device, &kb.device);

    let fd = device.as_raw_fd();
    let mut managed = ManagedDevice {
        device,
        path: kb.device.clone(),
        engine: MappingEngine::new(Arc::clone(lookup)),
        pending_scan: None,
        // The hot-plug thread cannot reach the virtual device, so the event
        // loop re-emits the held modifiers' key-downs on the device's first
        // event.
        pending_initial_state: true,
        pending_held_modifiers: Vec::new(),
    };
    // Same as at startup: capture the held keys at the grab instant, so a
    // key released before the event loop starts does not leak its
    // auto-repeat tail into the virtual device.
    let held_at_grab = capture_held_keys(&mut managed);
    // Same as at startup: let grab-time releases flow natively to the
    // compositor before the device joins the event loop.
    native_release_window(&mut managed, held_at_grab, drained > 0);

    // Register with managed devices.
    {
        let mut devices = managed_devices.lock();
        devices.push(managed);
    }

    // Register with epoll.
    if let Err(e) = epoll_add(epoll_fd, fd, fd as u64) {
        warn!("Failed to add {} to epoll: {e}", kb.device);
        // Rollback: remove from managed devices since epoll registration
        // failed.
        let mut devices = managed_devices.lock();
        if let Some(idx) = devices.iter().position(|m| m.path == kb.device) {
            devices.remove(idx);
        }
        return;
    }

    info!("Hot-plug: grabbed {} ({})", kb.device, kb.name);
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
            warn!("Remove event without devnode, skipping");
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
        warn!("Failed to remove {dev_path} from epoll: {e}");
    }

    info!("Hot-plug: removed {dev_path}");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::keyboard::KeyboardInfo;

    // -----------------------------------------------------------------------
    // Filter-aware hot-plug tests
    // -----------------------------------------------------------------------
    //
    // Verifies that the keyboard filter used by `handle_device_add` correctly
    // allows matching devices and blocks non-matching ones.  The hot-plug
    // handler uses `filter_keyboards_by_specifiers` to decide whether to
    // grab a newly discovered device.

    fn build_keyboard(
        name: &str,
        vendor: &str,
        model: &str,
        device: &str,
        port: Option<&str>,
    ) -> KeyboardInfo {
        KeyboardInfo::new(
            name.to_string(),
            vendor.to_string(),
            model.to_string(),
            device.to_string(),
            port.map(|s| s.to_string()),
        )
    }

    #[test]
    fn hotplug_filter_allows_matching_device() {
        // A filter that matches by vendor.
        let specs = vec![KeyboardSpecifier {
            name: None,
            vendor: Some("Logitech".to_string()),
            model: None,
            port: None,
        }];

        let kb = build_keyboard(
            "Logitech K800",
            "Logitech",
            "K800",
            "/dev/input/event5",
            Some("USB"),
        );

        let filtered = filter_keyboards_by_specifiers(
            std::slice::from_ref(&kb),
            Some(&specs),
        );
        assert_eq!(filtered.len(), 1, "matching device should be grabbed");
        assert_eq!(filtered[0].name, "Logitech K800");
    }

    #[test]
    fn hotplug_filter_blocks_non_matching_device() {
        // A filter that only matches "Logitech" vendor.
        let specs = vec![KeyboardSpecifier {
            name: None,
            vendor: Some("Logitech".to_string()),
            model: None,
            port: None,
        }];

        // A different vendor — should NOT be grabbed.
        let kb = build_keyboard(
            "Apple Magic Keyboard",
            "Apple",
            "Magic Keyboard",
            "/dev/input/event6",
            Some("Bluetooth"),
        );

        let filtered = filter_keyboards_by_specifiers(
            std::slice::from_ref(&kb),
            Some(&specs),
        );
        assert!(
            filtered.is_empty(),
            "non-matching device should NOT be grabbed"
        );
    }

    #[test]
    fn hotplug_no_filter_grabs_all_devices() {
        // When no global filter is set, all discovered devices are grabbed.
        let kb = build_keyboard(
            "Some Keyboard",
            "Generic",
            "Model X",
            "/dev/input/event7",
            None,
        );

        let filtered =
            filter_keyboards_by_specifiers(std::slice::from_ref(&kb), None);
        assert_eq!(filtered.len(), 1, "no filter should grab all devices");
    }

    #[test]
    fn hotplug_empty_filter_grabs_all_devices() {
        // An empty filter list is equivalent to no filter.
        let specs: Vec<KeyboardSpecifier> = vec![];

        let kb = build_keyboard(
            "Some Keyboard",
            "Generic",
            "Model X",
            "/dev/input/event8",
            None,
        );

        let filtered = filter_keyboards_by_specifiers(
            std::slice::from_ref(&kb),
            Some(&specs),
        );
        assert_eq!(filtered.len(), 1, "empty filter should grab all devices");
    }

    #[test]
    fn hotplug_filter_matches_by_name() {
        // Name matching is exact (case-insensitive), not substring.
        let specs = vec![KeyboardSpecifier {
            name: Some("Logitech K800".to_string()),
            vendor: None,
            model: None,
            port: None,
        }];

        let kb = build_keyboard(
            "Logitech K800",
            "Logitech",
            "K800",
            "/dev/input/event9",
            Some("USB"),
        );

        let filtered = filter_keyboards_by_specifiers(
            std::slice::from_ref(&kb),
            Some(&specs),
        );
        assert_eq!(filtered.len(), 1, "name filter should match");
    }

    #[test]
    fn hotplug_filter_matches_by_port() {
        let specs = vec![KeyboardSpecifier {
            name: None,
            vendor: None,
            model: None,
            port: Some("Bluetooth".to_string()),
        }];

        let kb_bluetooth = build_keyboard(
            "BT Keyboard",
            "Vendor",
            "Model",
            "/dev/input/event10",
            Some("Bluetooth"),
        );

        let kb_usb = build_keyboard(
            "USB Keyboard",
            "Vendor",
            "Model",
            "/dev/input/event11",
            Some("USB"),
        );

        let filtered_bt = filter_keyboards_by_specifiers(
            std::slice::from_ref(&kb_bluetooth),
            Some(&specs),
        );
        assert_eq!(filtered_bt.len(), 1, "Bluetooth device should match");

        let filtered_usb = filter_keyboards_by_specifiers(
            std::slice::from_ref(&kb_usb),
            Some(&specs),
        );
        assert!(filtered_usb.is_empty(), "USB device should NOT match");
    }

    // -----------------------------------------------------------------------
    // Restart-backoff tests
    // -----------------------------------------------------------------------
    //
    // The supervisor restarts the udev monitor when it exits unexpectedly.
    // The backoff must grow on consecutive failures (so a permanently
    // broken udev cannot spin) and reset after a healthy run.

    #[test]
    fn restart_delay_doubles_on_failure() {
        assert_eq!(
            next_restart_delay(RESTART_DELAY_MIN, false),
            RESTART_DELAY_MIN * 2
        );
    }

    #[test]
    fn restart_delay_caps_at_max() {
        // Doubling past the cap clamps to the cap and stays there.
        assert_eq!(
            next_restart_delay(RESTART_DELAY_MAX, false),
            RESTART_DELAY_MAX
        );
        let mut delay = RESTART_DELAY_MIN;
        for _ in 0..10 {
            delay = next_restart_delay(delay, false);
        }
        assert_eq!(delay, RESTART_DELAY_MAX);
    }

    #[test]
    fn restart_delay_resets_after_healthy_run() {
        assert_eq!(
            next_restart_delay(RESTART_DELAY_MAX, true),
            RESTART_DELAY_MIN
        );
    }
}
