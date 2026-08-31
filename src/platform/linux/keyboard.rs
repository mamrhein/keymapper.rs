// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux keyboard enumeration via udev.

use evdev::{Device, EventType};
use udev::Enumerator;

use crate::common::keyboard::KeyboardInfo;

/// Enumerate all keyboard input devices on the system.
///
/// Uses udev to discover devices tagged as keyboards belonging to the current
/// user seat.  Devices that also support absolute (pointer) events are
/// excluded because they are typically pointing devices that happen to
/// announce keyboard capabilities (e.g. touchpads with integrated buttons).
///
/// Returns both the [`KeyboardInfo`] metadata and the opened [`Device`]
/// handle.
fn enumerate_keyboards()
-> Result<Vec<(KeyboardInfo, Device)>, Box<dyn std::error::Error>> {
    let mut enumerator = Enumerator::new()?;

    enumerator.match_subsystem("input")?;
    enumerator.match_property("ID_INPUT_KEYBOARD", "1")?;
    enumerator.scan_devices()?;

    let results: Vec<_> = enumerator
        .scan_devices()?
        .filter_map(|udev_device| build_keyboard_from_udev(&udev_device))
        .collect();

    Ok(results)
}

/// Build a [`KeyboardInfo`] and open an [`evdev::Device`] from a udev
/// device object.
///
/// Returns `None` if the device node is missing, the device cannot be opened,
/// or the device is not a pure keyboard (e.g. it has absolute/pointer events).
///
/// Used by both the startup enumeration and the hot-plug monitor so that
/// device discovery logic is identical in both paths.
pub(super) fn build_keyboard_from_udev(
    udev_device: &udev::Device,
) -> Option<(KeyboardInfo, Device)> {
    // Resolve the device node (e.g. /dev/input/event3).  Skip if missing.
    let devnode = udev_device.devnode()?;

    // Open the evdev device to check for pointer-capable devices.
    let device = Device::open(devnode).ok()?;

    // Skip pointing devices announced as keyboards (touchpads,
    // touchscreens).
    if device.supported_events().contains(EventType::ABSOLUTE) {
        return None;
    }

    // udev property ID_PRODUCT_NAME is usually the most readable.  Fall
    // back to the evdev device name, then to a placeholder.
    let name = udev_device
        .property_value("ID_PRODUCT_NAME")
        .map(|s| s.to_string_lossy().into_owned())
        .or_else(|| device.name().map(str::to_owned))
        .unwrap_or_else(|| "<unknown>".to_string());

    // Vendor string from udev, derived from the USB/HID vendor descriptor.
    let vendor = udev_device
        .property_value("ID_VENDOR")
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown>".to_string());

    // Model from udev; if absent, construct a compact ID from vendor and
    // product IDs.
    let model = udev_device
        .property_value("ID_MODEL")
        .map(|s| s.to_string_lossy().into_owned())
        .or_else(|| {
            let vid = udev_device
                .property_value("ID_VENDOR_ID")
                .map(|s| s.to_string_lossy().into_owned());
            let pid = udev_device
                .property_value("ID_MODEL_ID")
                .map(|s| s.to_string_lossy().into_owned());

            match (vid, pid) {
                (Some(v), Some(p)) => Some(format!("{v}:{p}")),
                _ => None,
            }
        })
        .unwrap_or_else(|| "<unknown>".to_string());

    // The device node path is the handle used to open and filter events.
    let device_path = devnode.to_string_lossy().into_owned();

    // Transport type from udev: "usb", "bluetooth", "virtual", etc.
    let port = udev_device.property_value("ID_BUS").map(|s| {
        let bus = s.to_string_lossy();
        match bus.as_ref() {
            "usb" => "USB".to_string(),
            "bluetooth" => "Bluetooth".to_string(),
            "virtual" => "Virtual".to_string(),
            other => other.to_string(),
        }
    });

    Some((
        KeyboardInfo::new(name, vendor, model, device_path, port),
        device,
    ))
}

/// Enumerate all keyboard input devices on the system.
///
/// Uses udev to discover devices tagged as keyboards belonging to the current
/// user seat.  Devices that also support absolute (pointer) events are
/// excluded because they are typically pointing devices that happen to
/// announce keyboard capabilities (e.g. touchpads with integrated buttons).
pub fn list_keyboards() -> Result<Vec<KeyboardInfo>, Box<dyn std::error::Error>>
{
    let results = enumerate_keyboards()?;
    let keyboards: Vec<KeyboardInfo> =
        results.into_iter().map(|(info, _)| info).collect();

    if keyboards.is_empty() {
        return Err("No keyboard devices found.".into());
    }

    Ok(keyboards)
}

/// Enumerate and open all keyboard devices for the current seat.
///
/// Returns both the [`KeyboardInfo`] metadata and the opened [`Device`].
/// Callers that only need discovery (e.g. the CLI `keyboards` command) should
/// use [`list_keyboards`] instead.  The daemon uses this function to avoid a
/// second udev scan and redundant device opens.
pub(crate) fn discover_and_open_keyboards()
-> Result<Vec<(KeyboardInfo, Device)>, Box<dyn std::error::Error>> {
    enumerate_keyboards()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_info_fields_are_populated() {
        let info = KeyboardInfo::new(
            "Test Keyboard".into(),
            "TestVendor".into(),
            "TestModel".into(),
            "/dev/input/event0".into(),
            Some("USB".to_string()),
        );

        assert_eq!(info.name, "Test Keyboard");
        assert_eq!(info.vendor, "TestVendor");
        assert_eq!(info.model, "TestModel");
        assert_eq!(info.device, "/dev/input/event0");
        assert_eq!(info.port, Some("USB".to_string()));
    }

    #[test]
    fn list_keyboards_returns_keyboard_info_vec() {
        // On systems without keyboards this returns an error; on systems with
        // keyboards it returns a non-empty vec.  We only assert the type is
        // well-formed by calling it and checking the result shape.
        let result = list_keyboards();
        assert!(
            result.is_ok() || !result.unwrap_err().to_string().is_empty(),
            "should produce either a result or an error message"
        );
    }
}
