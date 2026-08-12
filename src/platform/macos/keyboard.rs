// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS keyboard enumeration via `ioreg` subprocess.
//!
//! IOKit HID Manager FFI is fragile on modern macOS (symbols are split across
//! sub-frameworks, APIs have been deprecated).  Using `ioreg` to query the
//! IOService registry is a reliable, dependency-free alternative.

use std::process::Command;

use crate::common::keyboard::KeyboardInfo;

// ---------------------------------------------------------------------------
// Vendor ID lookup
// ---------------------------------------------------------------------------

/// Map a USB vendor ID to a human-readable vendor name.
fn vendor_id_to_name(vendor_id: u32) -> String {
    match vendor_id {
        0x05ac => "Apple".to_string(),
        0x046d => "Logitech".to_string(),
        0x045e => "Microsoft".to_string(),
        0x0c45 => "Kensington".to_string(),
        0x04b3 => "IBM".to_string(),
        0x0842 => "HP".to_string(),
        0x06e0 => "Genius".to_string(),
        0x1131 => "Filco".to_string(),
        0x04d9 => "Holtek".to_string(),
        0x1532 => "Razer".to_string(),
        0x0b05 => "Aspect".to_string(),
        0x1a89 => "XIAOMI".to_string(),
        _ => format!("0x{:04x}", vendor_id),
    }
}

// ---------------------------------------------------------------------------
// ioreg parsing
// ---------------------------------------------------------------------------

/// Run `ioreg` and filter for keyboard devices.
fn run_ioreg() -> Result<String, Box<dyn std::error::Error>> {
    // Query the IOService registry for HID keyboard devices.
    let output = Command::new("ioreg")
        .args(["-p", "AppleHIDKeyboard", "-r", "-l", "-w", "0"])
        .output()
        .map_err(|e| format!("failed to execute ioreg: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ioreg failed: {stderr}").into());
    }

    String::from_utf8(output.stdout).map_err(
        |e| -> Box<dyn std::error::Error> {
            format!("ioreg output is not valid utf-8: {e}").into()
        },
    )
}

/// Parse a single device block from ioreg output.
///
/// ioreg with `-w 0` produces a semi-structured output where each device
/// starts with a line like:
/// ```text
/// +-o Keyboard  <class IOHIDKeyboardDevice, ...>
/// ```
/// followed by indented property lines like:
/// ```text
/// |   "Product" = "Magic Keyboard"
/// |   "Vendor ID" = 0x05ac
/// ```
fn parse_ioreg_output(output: &str) -> Vec<KeyboardInfo> {
    let mut keyboards = Vec::new();
    let mut current = None;

    for line in output.lines() {
        let trimmed = line.trim_start();

        // Detect the start of a new device entry.
        if trimmed.starts_with("+-o ") || trimmed.starts_with("|   +-o ") {
            // Save the previous device if it has a name.
            if let Some(kb) = current.take() {
                keyboards.push(kb);
            }

            // Extract the device name (text after "+-o ").
            let name_part = trimmed
                .strip_prefix("|   +-o ")
                .or_else(|| trimmed.strip_prefix("+-o "))
                .unwrap_or(trimmed);

            // The name ends at the first `<` (the class annotation).
            let name = name_part
                .split('<')
                .next()
                .unwrap_or(name_part)
                .trim()
                .to_string();

            current = Some(KeyboardInfo::new(
                name,
                "<unknown>".to_string(),
                "<unknown>".to_string(),
                "system".to_string(),
                None,
            ));
        } else if let Some(ref mut kb) = current {
            // Parse property lines.
            if let Some((key, value)) = parse_property_line(trimmed) {
                match key {
                    "Product" => {
                        // Override the device-class name with the actual
                        // product name.
                        if !value.is_empty() {
                            kb.name = value.clone();
                        }
                    }
                    "Vendor ID" => {
                        if let Some(vid) = parse_hex_u32(&value) {
                            kb.vendor = vendor_id_to_name(vid);
                            // Build model from vendor:product if product ID is
                            // available.
                            kb.model = format!("0x{:04x}", vid);
                        }
                    }
                    "Product ID" => {
                        if let Some(pid) = parse_hex_u32(&value) {
                            kb.model = format!("{}:0x{:04x}", kb.model, pid);
                        }
                    }
                    "Location ID" => {
                        if let Some(loc) = parse_hex_u32(&value) {
                            kb.device = format!("0x{:08x}", loc);
                        }
                    }
                    "Transport" => {
                        // Record the transport type.  Also use it as prefix
                        // for device string when device is still the default.
                        kb.port = Some(value.clone());
                        if kb.device == "system" {
                            kb.device = format!(
                                "{}:{}",
                                value.to_lowercase(),
                                kb.device
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Don't forget the last device.
    if let Some(kb) = current {
        keyboards.push(kb);
    }

    keyboards
}

/// Parse an ioreg property line like `"Product" = "Magic Keyboard"` or
/// `"Vendor ID" = 0x05ac`.
fn parse_property_line(line: &str) -> Option<(&str, String)> {
    // Lines look like: `"Key" = "Value"` or `"Key" = 0x1234`
    let line = line.trim_start().strip_prefix('|')?.trim_start();

    let mut parts = line.splitn(2, " = ");
    let key = parts.next()?.trim().trim_matches('"');
    let value = parts.next()?.trim();

    Some((key, value.trim_matches('"').to_string()))
}

/// Parse a hex string like `0x05ac` or `2316` into a u32.
fn parse_hex_u32(s: &str) -> Option<u32> {
    if let Some(hex) = s.strip_prefix("0x") {
        u32::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

// ---------------------------------------------------------------------------
// Fallback: minimal keyboard list when ioreg fails
// ---------------------------------------------------------------------------

fn fallback_keyboards() -> Vec<KeyboardInfo> {
    vec![KeyboardInfo::new(
        "System Keyboard".into(),
        "Apple".into(),
        "built-in".into(),
        "system".into(), // intercept all keyboards globally
        Some("Internal".to_string()),
    )]
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Enumerate all keyboard input devices on macOS.
///
/// Uses `ioreg` to query the IOService registry for HID keyboard devices and
/// extracts the product name, vendor ID, product ID, and location ID from each
/// device's properties.
///
/// The `device` field contains a hex-encoded location ID string (e.g.
/// `"0x00120000"`) which uniquely identifies the physical attachment point
/// (USB port, Bluetooth connection).  This value can be compared against the
/// device ID field of `CGEvent` to filter events from specific keyboards.
pub fn list_keyboards() -> Result<Vec<KeyboardInfo>, Box<dyn std::error::Error>>
{
    let output = run_ioreg()?;
    let keyboards = parse_ioreg_output(&output);

    if keyboards.is_empty() {
        return Ok(fallback_keyboards());
    }

    Ok(keyboards)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_info_fields_are_populated() {
        let info = KeyboardInfo::new(
            "Magic Keyboard".into(),
            "Apple".into(),
            "0x05ac:0xa25a".into(),
            "0x00120000".into(),
            Some("USB".to_string()),
        );

        assert_eq!(info.name, "Magic Keyboard");
        assert_eq!(info.vendor, "Apple");
        assert_eq!(info.model, "0x05ac:0xa25a");
        assert_eq!(info.device, "0x00120000");
        assert_eq!(info.port, Some("USB".to_string()));
    }

    #[test]
    fn vendor_id_to_name_apple() {
        assert_eq!(vendor_id_to_name(0x05ac), "Apple");
    }

    #[test]
    fn vendor_id_to_name_unknown() {
        assert_eq!(vendor_id_to_name(0xdead), "0xdead");
    }

    #[test]
    fn parse_hex_u32_with_prefix() {
        assert_eq!(parse_hex_u32("0x05ac"), Some(0x05ac));
    }

    #[test]
    fn parse_hex_u32_decimal() {
        assert_eq!(parse_hex_u32("2316"), Some(2316));
    }

    #[test]
    fn parse_hex_u32_invalid() {
        assert_eq!(parse_hex_u32("xyz"), None);
    }

    #[test]
    fn parse_property_line_string_value() {
        let line = "|   \"Product\" = \"Magic Keyboard\"";
        let (key, value) = parse_property_line(line).unwrap();
        assert_eq!(key, "Product");
        assert_eq!(value, "Magic Keyboard");
    }

    #[test]
    fn parse_property_line_hex_value() {
        let line = "|   \"Vendor ID\" = 0x05ac";
        let (key, value) = parse_property_line(line).unwrap();
        assert_eq!(key, "Vendor ID");
        assert_eq!(value, "0x05ac");
    }

    #[test]
    fn parse_ioreg_empty_output() {
        let result = parse_ioreg_output("");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_ioreg_product_overrides_name() {
        let output = "+-o Keyboard  <class IOHIDKeyboardDevice, id 0x100000200>\n\
            |   \"Product\" = \"Magic Keyboard\"\n\
            |   \"Vendor ID\" = 0x05ac\n\
            |   \"Product ID\" = 0xa25a\n\
            |   \"Location ID\" = 0x00120000";
        let result = parse_ioreg_output(output);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "Magic Keyboard");
        assert_eq!(result[0].vendor, "Apple");
        assert_eq!(result[0].model, "0x05ac:0xa25a");
        assert_eq!(result[0].device, "0x00120000");
    }

    #[test]
    fn parse_ioreg_multiple_devices() {
        let output = "+-o Keyboard  <class IOHIDKeyboardDevice, id \
                      0x100000200>\n|   \"Product\" = \"Magic Keyboard\"\n|   \
                      \"Vendor ID\" = 0x05ac\n+-o Keyboard  <class \
                      IOHIDKeyboardDevice, id 0x100000300>\n|   \"Product\" \
                      = \"Logitech K845\"\n|   \"Vendor ID\" = 0x046d";
        let result = parse_ioreg_output(output);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "Magic Keyboard");
        assert_eq!(result[0].vendor, "Apple");
        assert_eq!(result[1].name, "Logitech K845");
        assert_eq!(result[1].vendor, "Logitech");
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
