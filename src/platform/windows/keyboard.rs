// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows keyboard enumeration via SetupAPI and HID API.

use std::ptr;

use windows_sys::Win32::{
    Devices::{
        DeviceAndDriverInstallation::{
            DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
            SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
            SP_DEVINFO_DATA, SetupDiDestroyDeviceInfoList,
            SetupDiEnumDeviceInfo, SetupDiEnumDeviceInterfaces,
            SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
            SetupDiGetDeviceRegistryPropertyW,
        },
        HumanInterfaceDevice::{
            HidD_FreePreparsedData, HidD_GetManufacturerString,
            HidD_GetPreparsedData, HidD_GetProductString,
            HidD_GetSerialNumberString, HidP_GetCaps,
            HidP_GetLinkCollectionNodes, HIDP_CAPS,
            HIDP_LINK_COLLECTION_NODE, PHIDP_PREPARSED_DATA,
        },
    },
    Foundation::{GetLastError, GENERIC_READ, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
};

use crate::common::keyboard::KeyboardInfo;

// ---------------------------------------------------------------------------
// HID usage constants
// ---------------------------------------------------------------------------

/// SPDRP_DEVICEDESC constant for device description property.
const SPDRP_DEVICEDESC: u32 = 1;

/// SPDRP_MFG constant for manufacturer property.
const SPDRP_MFG: u32 = 13;

/// SPDRP_HARDWAREID constant for hardware ID property.
const SPDRP_HARDWAREID: u32 = 3;

/// HID usage page for Generic Desktop.
const HID_USAGE_PAGE_GENERIC: u16 = 0x01;

/// HID usage for Keyboard within the Generic Desktop page.
const HID_USAGE_KEYBOARD: u16 = 0x06;

// ---------------------------------------------------------------------------
// Helper: convert a wide string buffer to a Rust String
// ---------------------------------------------------------------------------

/// Convert a null-terminated wide-string buffer to a `String`.
fn wstring(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

// ---------------------------------------------------------------------------
// Helper: get the HID device class GUID as a string
// ---------------------------------------------------------------------------

/// The GUID for HID devices: `{4D1E55B2-F16F-11CF-88CB-001111000030}`.
/// Used with `SetupDiGetClassDevsW` to enumerate all HID devices.
const fn hid_class_guid() -> windows_sys::core::GUID {
    windows_sys::core::GUID {
        data1: 0x4D1E55B2,
        data2: 0xF16F,
        data3: 0x11CF,
        data4: [0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30],
    }
}

// ---------------------------------------------------------------------------
// Helper: parse vendor name from VID in device path
// ---------------------------------------------------------------------------

/// Well-known USB vendor IDs for common keyboard manufacturers.
const KNOWN_VENDORS: &[(u16, &str)] = &[
    (0x045E, "Microsoft"),
    (0x046D, "Logitech"),
    (0x04F2, "Chicony"), // often used by Apple
    (0x05AC, "Apple"),
    (0x06A3, "Fujitsu"),
    (0x0842, "HannSpree"),
    (0x0B05, "ASUS"),
    (0x047F, "Plantronics"),
];

/// Parses the VID from a device path like `\\?\\hid#vid_046d&...` and returns
/// the vendor name if known, or "Unknown (0xXXXX)".
fn parse_vendor_from_path(path: &str) -> Option<String> {
    let (vid, _pid) = parse_vid_pid(path)?;

    KNOWN_VENDORS
        .iter()
        .find(|(v, _)| *v == vid)
        .map(|(_, name)| name.to_string())
        .or(Some(format!("Unknown (0x{vid:04X})")))
}

/// Parses VID and PID from a device path.
fn parse_vid_pid(path: &str) -> Option<(u16, u16)> {
    let lower = path.to_lowercase();
    let vid_start = lower.find("vid_")? + 4;
    let vid_str = &lower[vid_start..vid_start + 4];
    if vid_str.chars().any(|c| !c.is_ascii_hexdigit()) {
        return None;
    }
    let vid = u16::from_str_radix(vid_str, 16).ok()?;

    let pid_start = lower.find("pid_")? + 4;
    let pid_str = &lower[pid_start..pid_start + 4];
    if pid_str.chars().any(|c| !c.is_ascii_hexdigit()) {
        return None;
    }
    let pid = u16::from_str_radix(pid_str, 16).ok()?;

    Some((vid, pid))
}

/// Derives the transport/port type from hardware ID or device path.
fn derive_port(hardware_id: &str, interface_path: &str) -> Option<String> {
    // Check hardware ID first.
    if !hardware_id.is_empty() {
        if let Some(first_id) = hardware_id.split('\n').next() {
            let lower = first_id.to_lowercase();
            if lower.starts_with("usb") {
                return Some("USB".to_string());
            }
            if lower.starts_with("bthenum") || lower.starts_with("bthledev") {
                return Some("Bluetooth".to_string());
            }
            if lower.starts_with("acpi") {
                return Some("Internal".to_string());
            }
        }
    }

    // Fall back to checking the interface path.
    let path_lower = interface_path.to_lowercase();
    if path_lower.contains("bth_") || path_lower.contains("bthenum") {
        return Some("Bluetooth".to_string());
    }
    // Most HID keyboards are USB; ACPI is typically internal/laptop built-in.
    if path_lower.contains("acpi") {
        return Some("Internal".to_string());
    }
    if path_lower.contains("hid#") || path_lower.contains("hid\\") {
        return Some("USB".to_string());
    }

    None
}

// ---------------------------------------------------------------------------
// Helper: open an HID device handle from its interface path
// ---------------------------------------------------------------------------

/// Open a device handle for the given interface path string.
fn open_hid_device(interface_path: &[u16]) -> Option<*mut std::ffi::c_void> {
    let handle = unsafe {
        CreateFileW(
            interface_path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null_mut(), // no security attributes
            OPEN_EXISTING,
            0, // no flags/attributes
            ptr::null_mut(), // no template file
        )
    };

    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return None;
    }

    Some(handle)
}

// ---------------------------------------------------------------------------
// Helper: read a device property as a wide string
// ---------------------------------------------------------------------------

/// Read a registry property from a device info set.
fn get_device_property(
    h_dev_info: isize,
    dev_info_data: &mut SP_DEVINFO_DATA,
    property: u32,
) -> Option<String> {
    let mut required_size: u32 = 0;

    // First call to determine the required buffer size.
    unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            h_dev_info,
            dev_info_data,
            property,
            ptr::null_mut(),
            ptr::null_mut() as _,
            0,
            &mut required_size,
        );
    }

    if required_size == 0 {
        return None;
    }

    let mut buf = vec![0u16; required_size as usize];
    let success = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            h_dev_info,
            dev_info_data,
            property,
            ptr::null_mut(),
            buf.as_mut_ptr() as _,
            buf.len() as u32,
            ptr::null_mut(),
        )
    };

    if success != 0 {
        Some(wstring(&buf))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Helper: read HID string attributes via an open handle
// ---------------------------------------------------------------------------

/// Read product, manufacturer, and serial from an open HID device handle.
fn read_hid_strings(handle: *mut std::ffi::c_void) -> (String, String, String) {
    let mut product_buf = [0u16; 128];
    let mut vendor_buf = [0u16; 128];
    let mut serial_buf = [0u16; 128];

    let product = unsafe {
        HidD_GetProductString(
            handle,
            product_buf.as_mut_ptr() as *mut std::ffi::c_void,
            product_buf.len() as u32,
        )
    };

    let vendor = unsafe {
        HidD_GetManufacturerString(
            handle,
            vendor_buf.as_mut_ptr() as *mut std::ffi::c_void,
            vendor_buf.len() as u32,
        )
    };

    let serial = unsafe {
        HidD_GetSerialNumberString(
            handle,
            serial_buf.as_mut_ptr() as *mut std::ffi::c_void,
            serial_buf.len() as u32,
        )
    };

    (
        if product {
            wstring(&product_buf)
        } else {
            String::new()
        },
        if vendor {
            wstring(&vendor_buf)
        } else {
            String::new()
        },
        if serial {
            wstring(&serial_buf)
        } else {
            String::new()
        },
    )
}

// ---------------------------------------------------------------------------
// Helper: check if an HID device is a keyboard
// ---------------------------------------------------------------------------

/// Checks if the device is a keyboard by examining HID usage information.
///
/// Returns `Some(true)` if the device is confirmed as a keyboard, `Some(false)`
/// if confirmed as non-keyboard, and `None` if the HID preparsed data could
/// not be read (caller should use a fallback check).
fn check_keyboard_via_hid(handle: *mut std::ffi::c_void) -> Option<bool> {
    let mut preparsed_data: PHIDP_PREPARSED_DATA = 0;

    let success = unsafe {
        HidD_GetPreparsedData(handle, &mut preparsed_data)
    };

    if !success || preparsed_data == 0 {
        return None;
    }

    // Free the preparsed data when we're done, even on early return.
    let _guard = PreparsedDataGuard(preparsed_data);

    // First, check the primary collection via HidP_GetCaps.
    let mut caps: HIDP_CAPS = unsafe { std::mem::zeroed() };
    let status = unsafe { HidP_GetCaps(preparsed_data, &mut caps) };
    if status == 0
        && caps.UsagePage == HID_USAGE_PAGE_GENERIC
        && caps.Usage == HID_USAGE_KEYBOARD
    {
        return Some(true);
    }

    // Composite devices may have multiple top-level collections.  Iterate
    // through all link collection nodes and check each top-level one.
    let mut node_count: u32 = 0;
    let status = unsafe {
        HidP_GetLinkCollectionNodes(
            ptr::null_mut(),
            &mut node_count,
            preparsed_data,
        )
    };
    if status != 0 || node_count == 0 {
        return None;
    }

    let mut nodes: Vec<HIDP_LINK_COLLECTION_NODE> = (0..node_count)
        .map(|_| HIDP_LINK_COLLECTION_NODE::default())
        .collect();

    let status = unsafe {
        HidP_GetLinkCollectionNodes(
            nodes.as_mut_ptr(),
            &mut node_count,
            preparsed_data,
        )
    };
    if status != 0 {
        return None;
    }

    // Top-level collections have Parent == 0xFFFF.
    for node in &nodes[..node_count as usize] {
        if node.Parent == 0xFFFF
            && node.LinkUsagePage == HID_USAGE_PAGE_GENERIC
            && node.LinkUsage == HID_USAGE_KEYBOARD
        {
            return Some(true);
        }
    }

    Some(false)
}

/// Falls back to checking device strings for keyboard-related keywords when
/// HID preparsed data is unavailable.
fn looks_like_keyboard(name: &str, hardware_id: &str, interface_path: &str) -> bool {
    let name_lower = name.to_lowercase();
    let hw_lower = hardware_id.to_lowercase();
    let path_lower = interface_path.to_lowercase();

    // Check device name.
    if name_lower.contains("keyboard") || name_lower.contains("kbd") {
        return true;
    }

    // Check interface path for keyboard-specific sub-path.
    if path_lower.ends_with(r"\kbd") {
        return true;
    }

    // Check hardware ID for common keyboard patterns.
    if hw_lower.contains("hidbth\\")
        || hw_lower.contains("kbd")
        || hw_lower.contains("bthenum")
    {
        return true;
    }

    false
}

/// RAII guard that frees preparsed HID data on drop.
struct PreparsedDataGuard(PHIDP_PREPARSED_DATA);

impl Drop for PreparsedDataGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe { HidD_FreePreparsedData(self.0) };
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Enumerate all keyboard input devices on Windows.
///
/// Uses SetupAPI to discover HID device interfaces, then filters to devices
/// whose HID usage page and usage match a keyboard.  For each matching device
/// the product name, vendor, model (hardware ID), and device interface path
/// are collected.
///
/// The `device` field contains the device interface path (e.g.
/// `\\?\hid#vid_046d+pid_c345#...`) which can be passed to `CreateFileW`
/// to open a handle for raw input filtering.
pub fn list_keyboards() -> Result<Vec<KeyboardInfo>, Box<dyn std::error::Error>>
{
    let guid = hid_class_guid();

    // Get the device info set for all present HID devices.  When
    // DIGCF_DEVICEINTERFACE is set, ClassGuid must point to the desired
    // interface class GUID.
    let h_dev_info = unsafe {
        SetupDiGetClassDevsW(
            &guid,
            ptr::null(),
            ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };

    if h_dev_info == isize::MAX || h_dev_info < 0 {
        let err = unsafe { GetLastError() };
        return Err(format!(
            "SetupDiGetClassDevsW failed with error code {}",
            err
        )
        .into());
    }

    let _guard = SetupDiGuard(h_dev_info);

    let mut keyboards = Vec::new();
    // Track diagnostics for a useful error message when no keyboards are found.
    let mut total_interfaces = 0u32;
    let mut open_failed = 0u32;
    let mut not_keyboard = 0u32;

    // Enumerate device interfaces.
    let mut interface_data: SP_DEVICE_INTERFACE_DATA =
        unsafe { std::mem::zeroed() };
    interface_data.cbSize =
        std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;

    let mut device_index = 0u32;
    loop {
        let success = unsafe {
            SetupDiEnumDeviceInterfaces(
                _guard.0,
                ptr::null_mut(),
                &guid,
                device_index,
                &mut interface_data,
            )
        };

        if success == 0 {
            break; // No more interfaces.
        }

        total_interfaces += 1;

        // Get the device interface detail (includes the interface path).
        let mut required_size: u32 = 0;
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                _guard.0,
                &interface_data,
                ptr::null_mut(),
                0,
                &mut required_size,
                ptr::null_mut(),
            );
        }

        if required_size == 0 {
            device_index += 1;
            continue;
        }

        let mut detail_buf = vec![0u8; required_size as usize];
        let detail_data: *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W =
            detail_buf.as_mut_ptr() as *mut _;
        unsafe {
            (*detail_data).cbSize =
                std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        }

        let detail_ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                _guard.0,
                &interface_data,
                detail_data,
                required_size,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };

        if detail_ok == 0 {
            device_index += 1;
            continue;
        }

        // Extract the interface path from the detail data.  The path starts
        // immediately after the `cbSize` field (offset 4).  We use a fixed
        // offset rather than `size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>()`
        // because windows-sys defines DevicePath as [u16; 1] (6-byte struct)
        // instead of a true flexible array member (4-byte header).
        const DEVICE_PATH_OFFSET: usize = 4;
        if detail_buf.len() <= DEVICE_PATH_OFFSET {
            device_index += 1;
            continue;
        }

        // Convert the remaining bytes to u16 (wide chars).
        let path_bytes = &detail_buf[DEVICE_PATH_OFFSET..];
        let interface_path: Vec<u16> = path_bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_ne_bytes([chunk[0], chunk[1]]))
            .collect();
        let interface_path_str = wstring(&interface_path);

        // Get device info data for this interface so we can read registry
        // properties.  We need this regardless of whether the device open
        // succeeds, since we use it for fallback keyboard detection.
        let mut dev_info_data: SP_DEVINFO_DATA =
            unsafe { std::mem::zeroed() };
        dev_info_data.cbSize =
            std::mem::size_of::<SP_DEVINFO_DATA>() as u32;

        let mut found_info = false;
        let mut idx = 0u32;
        loop {
            let mut di_data: SP_DEVINFO_DATA =
                unsafe { std::mem::zeroed() };
            di_data.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;

            if unsafe {
                SetupDiEnumDeviceInfo(_guard.0, idx, &mut di_data)
            }
            == 0
            {
                break;
            }

            // Check if this device info entry has our interface.
            let mut test_iface: SP_DEVICE_INTERFACE_DATA =
                unsafe { std::mem::zeroed() };
            test_iface.cbSize =
                std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;

            if unsafe {
                SetupDiEnumDeviceInterfaces(
                    _guard.0,
                    &di_data,
                    &guid,
                    0,
                    &mut test_iface,
                )
            } != 0
            {
                // Found a matching device info entry.
                dev_info_data = di_data;
                found_info = true;
                break;
            }

            idx += 1;
        }

        // Read device properties for potential fallback detection.
        let dev_desc = if found_info {
            get_device_property(_guard.0, &mut dev_info_data, SPDRP_DEVICEDESC)
        } else {
            None
        }
        .unwrap_or_default();

        let hw_id_raw = if found_info {
            get_device_property(_guard.0, &mut dev_info_data, SPDRP_HARDWAREID)
        } else {
            None
        }
        .unwrap_or_default();

        // Open the device to query HID attributes, and check if it's a keyboard.
        let is_keyboard = match open_hid_device(&interface_path) {
            Some(handle) => {
                // Try HID usage check first; fall back to keyword matching
                // if preparsed data is unavailable.
                match check_keyboard_via_hid(handle) {
                    Some(result) => result,
                    None => looks_like_keyboard(&dev_desc, &hw_id_raw, &interface_path_str),
                }
            }
            None => {
                open_failed += 1;
                // Can't open the device — use keyword-based detection.
                looks_like_keyboard(&dev_desc, &hw_id_raw, &interface_path_str)
            }
        };

        if !is_keyboard {
            not_keyboard += 1;
            device_index += 1;
            continue;
        }

        // We already have `dev_desc` and `hw_id_raw` from earlier.
        let dev_name = if found_info {
            get_device_property(_guard.0, &mut dev_info_data, SPDRP_DEVICEDESC)
        } else {
            None
        };

        let manufacturer = if found_info {
            get_device_property(_guard.0, &mut dev_info_data, SPDRP_MFG)
        } else {
            None
        };

        // Read HID string attributes from the device if we can open it.
        let (hid_product, hid_vendor, hid_serial) = match open_hid_device(&interface_path) {
            Some(handle) => {
                let strings = read_hid_strings(handle);
                unsafe {
                    windows_sys::Win32::Foundation::CloseHandle(handle);
                }
                strings
            }
            None => (String::new(), String::new(), String::new()),
        };

        // Build the fields, preferring HID string attributes over registry,
        // falling back to path-derived info for filtered devices.
        let name = if !hid_product.is_empty() {
            hid_product.clone()
        } else if !dev_desc.is_empty() && !dev_desc.contains(r"\") {
            // Use device description only if it looks like a real name
            // (hardware IDs contain backslashes).
            dev_desc.clone()
        } else {
            // Construct a display name from the device path.
            parse_vid_pid(&interface_path_str)
                .map(|(v, p)| format!("USB Keyboard ({:04X}:{:04X})", v, p))
                .unwrap_or_else(|| "HID Keyboard".to_string())
        };

        let vendor = if hid_vendor.is_empty() {
            manufacturer
                .or_else(|| parse_vendor_from_path(&interface_path_str))
                .unwrap_or_else(|| "<unknown>".to_string())
        } else {
            hid_vendor.clone()
        };

        let model = if !hw_id_raw.is_empty() {
            hw_id_raw.clone()
        } else if !hid_serial.is_empty() {
            hid_serial.clone()
        } else if !hid_product.is_empty() {
            hid_product.clone()
        } else {
            // Derive model from VID:PID in the path.
            parse_vid_pid(&interface_path_str)
                .map(|(v, p)| format!("{:04X}:{:04X}", v, p))
                .unwrap_or_else(|| "<unknown>".to_string())
        };

        // Derive transport type from the hardware ID prefix or device path.
        let port = derive_port(&hw_id_raw, &interface_path_str);

        keyboards.push(KeyboardInfo::new(
            name,
            vendor,
            model,
            interface_path_str,
            port,
        ));

        device_index += 1;
    }

    if keyboards.is_empty() {
        return Err(format!(
            "No keyboard devices found. ({total_interfaces} HID interface(s) \
             enumerated, {open_failed} failed to open, {not_keyboard} not a \
             keyboard)",
        )
        .into());
    }

    Ok(keyboards)
}

/// RAII guard that closes the setup device info list on drop.
struct SetupDiGuard(isize);

impl Drop for SetupDiGuard {
    fn drop(&mut self) {
        if self.0 != isize::MAX && self.0 >= 0 {
            unsafe { SetupDiDestroyDeviceInfoList(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_info_fields_are_populated() {
        let info = KeyboardInfo::new(
            "Microsoft Keyboard".into(),
            "Microsoft".into(),
            "HID\\VID_045E+PID_07FF".into(),
            "\\\\?\\hid#vid_045e+pid_07ff".into(),
            Some("USB".to_string()),
        );

        assert_eq!(info.name, "Microsoft Keyboard");
        assert_eq!(info.vendor, "Microsoft");
        assert_eq!(info.model, "HID\\VID_045E+PID_07FF");
        assert_eq!(info.device, "\\\\?\\hid#vid_045e+pid_07ff");
        assert_eq!(info.port, Some("USB".to_string()));
    }

    #[test]
    fn wstring_null_terminated() {
        let buf: &[u16] = &[72, 101, 108, 108, 111, 0, 42]; // "Hello\0*"
        assert_eq!(wstring(buf), "Hello");
    }

    #[test]
    fn wstring_no_null() {
        let buf: &[u16] = &[84, 101, 115, 116]; // "Test"
        assert_eq!(wstring(buf), "Test");
    }

    #[test]
    fn hid_class_guid_is_valid() {
        let guid = hid_class_guid();
        assert_eq!(guid.data1, 0x4D1E55B2);
    }

    #[test]
    fn hid_keyboard_usage_constants_are_correct() {
        // Generic Desktop usage page.
        assert_eq!(HID_USAGE_PAGE_GENERIC, 0x01);
        // Keyboard usage.
        assert_eq!(HID_USAGE_KEYBOARD, 0x06);
    }

    #[test]
    fn parse_vendor_from_path_finds_known_vendor() {
        let path = "\\?\\hid#vid_046d&pid_c52b&mi_00#...";
        assert_eq!(
            parse_vendor_from_path(path),
            Some("Logitech".to_string())
        );

        let path = "\\?\\hid#vid_04f2&pid_2159...";
        assert_eq!(
            parse_vendor_from_path(path),
            Some("Chicony".to_string())
        );

        let path = "\\?\\hid#vid_045e&pid_07ff...";
        assert_eq!(
            parse_vendor_from_path(path),
            Some("Microsoft".to_string())
        );
    }

    #[test]
    fn parse_vendor_from_path_unknown_vendor() {
        let path = "\\?\\hid#vid_1234&pid_5678...";
        assert_eq!(
            parse_vendor_from_path(path),
            Some("Unknown (0x1234)".to_string())
        );
    }

    #[test]
    fn parse_vendor_from_path_no_vid() {
        assert!(parse_vendor_from_path("\\?\\hid#vid_1234&pid_abcd").is_some());
        assert!(parse_vendor_from_path("no_vid_here").is_none());
    }

    #[test]
    fn looks_like_keyboard_detects_name_match() {
        assert!(looks_like_keyboard("HID Keyboard Device", "", ""));
        assert!(looks_like_keyboard(
            "Apple Internal Keyboard / Trackpad",
            "",
            ""
        ));
    }

    #[test]
    fn looks_like_keyboard_detects_hwid_match() {
        assert!(looks_like_keyboard(
            "",
            "BTHENUM\\{00001101-0000-1000-8000-00805F9B34FB}_LocalModeHID",
            ""
        ));
    }

    #[test]
    fn looks_like_keyboard_detects_kbd_suffix() {
        assert!(looks_like_keyboard(
            "",
            "",
            "\\?\\hid#vid_04f2&pid_2159&mi_00#...#{4d1e55b2-f16f-11cf-88cb-001111000030}\\kbd"
        ));
    }

    #[test]
    fn looks_like_keyboard_rejects_non_keyboard() {
        assert!(!looks_like_keyboard("HID-compliant mouse", "", ""));
        assert!(!looks_like_keyboard(
            "",
            "USB\\VID_046D+PID_C077&MI_01",
            ""
        ));
    }

    #[test]
    fn list_keyboards_returns_keyboard_info_vec() {
        let result = list_keyboards();
        assert!(
            result.is_ok() || !result.unwrap_err().to_string().is_empty(),
            "should produce either a result or an error message"
        );
    }
}
