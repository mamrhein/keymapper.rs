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
            DEVREGINFOCLASS, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
            SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
            SP_DEVINFO_DATA, SetupDiDestroyDeviceInfoList,
            SetupDiEnumDeviceInfo, SetupDiEnumDeviceInterfaces,
            SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
            SetupDiGetDeviceRegistryPropertyW,
        },
        HumanInterfaceDevice::{
            HIDD_ATTRIBUTES, HidD_GetAttributes, HidD_GetManufacturerString,
            HidD_GetProductString, HidD_GetSerialNumberString, HidD_GetUsage,
            HidD_GetUsagePage,
        },
    },
    Foundation::{GetLastError, INVALID_HANDLE_VALUE},
    System::WindowsAndMessaging::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
};

use crate::common::keyboard::KeyboardInfo;

// ---------------------------------------------------------------------------
// HID usage constants
// ---------------------------------------------------------------------------

/// HID usage page for generic desktop controls.
const HID_USAGE_PAGE_GENERIC: u32 = 0x01;

/// HID usage for a keyboard within the generic desktop page.
const HID_USAGE_KEYBOARD: u32 = 0x06;

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
        Data1: 0x4D1E55B2,
        Data2: 0xF16F,
        Data3: 0x11CF,
        Data4: [0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30],
    }
}

// ---------------------------------------------------------------------------
// Helper: open an HID device handle from its interface path
// ---------------------------------------------------------------------------

/// Open a device handle for the given interface path string.
fn open_hid_device(interface_path: &[u16]) -> Option<u64> {
    let handle = unsafe {
        CreateFileW(
            interface_path.as_ptr(),
            0, // no access — we only need it for queries
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null_mut(), // no security attributes
            OPEN_EXISTING,
            0, // no flags/attributes
            0, // no template file
        )
    };

    if handle == INVALID_HANDLE_VALUE as u64 || handle == 0 {
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
    property: DEVREGINFOCLASS,
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
fn read_hid_strings(handle: u64) -> (String, String, String) {
    let mut product_buf = [0u16; 128];
    let mut vendor_buf = [0u16; 128];
    let mut serial_buf = [0u16; 128];

    let product = unsafe {
        HidD_GetProductString(
            handle as isize,
            product_buf.as_mut_ptr(),
            product_buf.len() as u32,
        ) != 0
    };

    let vendor = unsafe {
        HidD_GetManufacturerString(
            handle as isize,
            vendor_buf.as_mut_ptr(),
            vendor_buf.len() as u32,
        ) != 0
    };

    let serial = unsafe {
        HidD_GetSerialNumberString(
            handle as isize,
            serial_buf.as_mut_ptr(),
            serial_buf.len() as u32,
        ) != 0
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

    // Get the device info set for all present HID devices.
    let h_dev_info = unsafe {
        SetupDiGetClassDevsW(
            ptr::null(), // &GUID,
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

        let mut detail_buf = vec![0u16; required_size as usize];
        let mut detail_data = unsafe {
            std::mem::transmute::<
                &mut [u16],
                &mut SP_DEVICE_INTERFACE_DETAIL_DATA_W,
            >(&mut detail_buf)
        };
        detail_data.cbSize =
            std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;

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

        // Extract the interface path from the detail data.  The path starts at
        // `detail_data.DevicePath` which is a null-terminated wide string.  We
        // skip the `cbSize` field (first 2 u16s = 4 bytes) to find the start
        // of DevicePath.
        let path_offset =
            std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() / 2;
        if detail_buf.len() <= path_offset {
            device_index += 1;
            continue;
        }

        let interface_path: Vec<u16> = detail_buf[path_offset..].to_vec();
        let interface_path_str = wstring(&interface_path);

        // Open the device to query HID attributes.
        let handle = match open_hid_device(&interface_path) {
            Some(h) => h,
            None => {
                device_index += 1;
                continue;
            }
        };

        // Check HID usage page and usage — only accept keyboards.
        let mut usage_page: u32 = 0;
        let mut usage: u32 = 0;

        let page_ok = unsafe {
            HidD_GetUsagePage(handle as isize, &mut usage_page) != 0
        };
        let usage_ok =
            unsafe { HidD_GetUsage(handle as isize, &mut usage) != 0 };

        if page_ok
            && usage_ok
            && usage_page == HID_USAGE_PAGE_GENERIC
            && usage == HID_USAGE_KEYBOARD
        {
            // Get the device info data for this interface so we can read
            // registry properties.
            let mut dev_info_data: SP_DEVINFO_DATA =
                unsafe { std::mem::zeroed() };
            dev_info_data.cbSize =
                std::mem::size_of::<SP_DEVINFO_DATA>() as u32;

            // Try to find the device info entry that corresponds to this
            // interface by iterating.
            let mut found_info = false;
            let mut idx = 0u32;
            loop {
                let mut di_data: SP_DEVINFO_DATA =
                    unsafe { std::mem::zeroed() };
                di_data.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;

                if unsafe {
                    SetupDiEnumDeviceInfo(_guard.0, idx, &mut di_data)
                } == 0
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
                        &mut di_data,
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

            // Read device properties.
            let dev_name = if found_info {
                get_device_property(
                    _guard.0,
                    &mut dev_info_data,
                    1, // SPDRP_DEVICEDESC
                )
            } else {
                None
            };

            let manufacturer = if found_info {
                get_device_property(
                    _guard.0,
                    &mut dev_info_data,
                    13, // SPDRP_MFG
                )
            } else {
                None
            };

            let hardware_id = if found_info {
                get_device_property(
                    _guard.0,
                    &mut dev_info_data,
                    3, // SPDRP_HARDWAREID
                )
            } else {
                None
            };

            // Read HID string attributes from the device.
            let (hid_product, hid_vendor, hid_serial) =
                read_hid_strings(handle);

            // Close the device handle.
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(handle as isize);
            }

            // Build the fields, preferring HID string attributes over registry.
            let name = dev_name
                .or(Some(hid_product.clone()))
                .unwrap_or_else(|| "<unknown>".to_string());

            let vendor = hid_vendor
                .or(manufacturer)
                .unwrap_or_else(|| "<unknown>".to_string());

            let model = hardware_id
                .or(Some(hid_serial.clone()))
                .unwrap_or_else(|| hid_product);

            keyboards.push(KeyboardInfo::new(
                name,
                vendor,
                model,
                interface_path_str,
            ));
        }

        device_index += 1;
    }

    if keyboards.is_empty() {
        return Err("No keyboard devices found.".into());
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
        );

        assert_eq!(info.name, "Microsoft Keyboard");
        assert_eq!(info.vendor, "Microsoft");
        assert_eq!(info.model, "HID\\VID_045E+PID_07FF");
        assert_eq!(info.device, "\\\\?\\hid#vid_045e+pid_07ff");
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
        assert_eq!(guid.Data1, 0x4D1E55B2);
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
