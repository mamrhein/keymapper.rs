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
    sync::Arc,
    thread,
};

use libc::c_int;
use parking_lot::Mutex;
use udev::{Enumerator, MonitorBuilder};

use super::{
    VIRTUAL_KEYBOARD_NAME,
    device::ManagedDevice,
    epoll::{epoll_add, epoll_del},
};
use crate::{
    common::keyboard::{KeyboardSpecifier, filter_keyboards_by_specifiers},
    platform::linux::keyboard::build_keyboard_from_udev,
};

/// Spawn a background thread that listens for keyboard device add/remove
/// events via udev and dynamically updates the managed device set.
///
/// New devices are only grabbed if they match the global keyboard filter.
/// Removed devices are ungrabbed and removed from the epoll set.
///
/// **Limitation:** changes to the global `keyboards:` filter at runtime do
/// not affect the grab list. The user must restart the daemon for
/// filter changes to take effect on hot-plugged devices.
pub(super) fn start_hotplug_monitor(
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

            // Resync: the startup udev snapshot in `start_mapping` and this
            // monitor's `listen()` call are not atomic.  A keyboard added in
            // between (e.g. a test injector created moments before daemon
            // start, or a device whose udev tagging finished after the
            // snapshot) never emits a fresh "add" event to this monitor and
            // would otherwise never be grabbed.  Rescan and adopt any
            // missing keyboards to close that window.
            resync_devices(&managed_devices, epoll_fd, &global_filter);

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

/// Rescan udev for keyboards that are not yet managed and adopt them.
///
/// Called once after the hot-plug monitor starts listening, to cover the
/// race window between the startup snapshot and the monitor's `listen()`
/// call.  Reuses [`handle_device_add`], which skips devices that are
/// already managed.
fn resync_devices(
    managed_devices: &Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
    global_filter: &Option<Vec<KeyboardSpecifier>>,
) {
    let Ok(mut enumerator) = Enumerator::new() else {
        eprintln!("warning: resync: failed to create udev enumerator");
        return;
    };

    if enumerator.match_subsystem("input").is_err()
        || enumerator.match_property("ID_INPUT_KEYBOARD", "1").is_err()
    {
        eprintln!("warning: resync: failed to configure udev enumerator");
        return;
    }

    let Ok(devices) = enumerator.scan_devices() else {
        eprintln!("warning: resync: failed to scan udev devices");
        return;
    };

    for udev_device in devices {
        handle_device_add(
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
        forwarded_modifiers: 0,
        consumed_modifiers: 0,
        pending_scan: None,
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
}
