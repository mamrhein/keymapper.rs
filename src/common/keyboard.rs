// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Platform-agnostic keyboard device metadata and filtering specifiers.

use serde::{Deserialize, Serialize};

/// Metadata for a single detected keyboard device.
#[derive(Debug, Clone)]
pub struct KeyboardInfo {
    /// Human-readable product name (e.g. "Logitech K845").
    pub name: String,

    /// Vendor or manufacturer string (e.g. "Logitech").
    pub vendor: String,

    /// Model identifier (e.g. "K845" or a vendor+product ID string).
    pub model: String,

    /// Platform-specific device identifier usable to filter key events.
    pub device: String,

    /// Transport / port type indicating how the device is connected (e.g.
    /// "USB", "Bluetooth", "Internal").  `None` when the platform cannot
    /// determine or expose this information.
    pub port: Option<String>,
}

impl KeyboardInfo {
    /// Create a new keyboard info record.
    pub fn new(
        name: String,
        vendor: String,
        model: String,
        device: String,
        port: Option<String>,
    ) -> Self {
        Self {
            name,
            vendor,
            model,
            device,
            port,
        }
    }
}

/// A partial keyboard identifier used in configuration to filter events by
/// device.
///
/// Any combination of `name`, `vendor`, `model`, and `port` can be provided.
/// A specifier matches a keyboard when **all** provided fields match.  Fields
/// comparison is case-insensitive to tolerate vendor name variations.
///
/// Multiple specifiers in a list form an OR set — matching any one is
/// sufficient.  An empty specifier (all fields `None`) is invalid and should
/// be rejected at configuration validation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyboardSpecifier {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
}

impl KeyboardSpecifier {
    /// Returns `true` when no fields are set.  Such a specifier is
    /// meaningless and should be rejected during configuration validation.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.vendor.is_none()
            && self.model.is_none()
            && self.port.is_none()
    }

    /// Returns `true` when at least one provided field matches the
    /// corresponding field on the keyboard.  All set fields must match.
    pub fn matches(&self, info: &KeyboardInfo) -> bool {
        if let Some(ref n) = self.name
            && !str_matches_ignore_case(n, &info.name)
        {
            return false;
        }
        if let Some(ref v) = self.vendor
            && !str_matches_ignore_case(v, &info.vendor)
        {
            return false;
        }
        if let Some(ref m) = self.model
            && !str_matches_ignore_case(m, &info.model)
        {
            return false;
        }
        if let Some(ref p) = self.port {
            match &info.port {
                Some(device_port) => {
                    if !str_matches_ignore_case(p, device_port) {
                        return false;
                    }
                }
                None => {
                    // The device has no port info; a port filter cannot match.
                    return false;
                }
            }
        }
        true
    }
}

/// Case-insensitive string comparison used for keyboard field matching.
fn str_matches_ignore_case(pattern: &str, value: &str) -> bool {
    pattern.eq_ignore_ascii_case(value)
}

/// Filter a list of discovered keyboards against the global keyboard
/// specifiers.
///
/// Returns all keyboards when `filter` is `None` or empty.  When set, returns
/// only keyboards that match at least one specifier.  Keyboards that are not
/// in the resulting list will not be grabbed by the daemon.
pub fn filter_keyboards_by_specifiers(
    keyboards: &[KeyboardInfo],
    filter: Option<&[KeyboardSpecifier]>,
) -> Vec<KeyboardInfo> {
    let Some(specs) = filter else {
        return keyboards.to_vec();
    };
    if specs.is_empty() {
        return keyboards.to_vec();
    }

    keyboards
        .iter()
        .filter(|kb| specs.iter().any(|spec| spec.matches(kb)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            port.map(str::to_string),
        )
    }

    #[test]
    fn is_empty_with_no_fields() {
        let spec = KeyboardSpecifier {
            name: None,
            vendor: None,
            model: None,
            port: None,
        };
        assert!(spec.is_empty());
    }

    #[test]
    fn is_empty_with_any_field_set() {
        let spec = KeyboardSpecifier {
            name: Some("test".to_string()),
            vendor: None,
            model: None,
            port: None,
        };
        assert!(!spec.is_empty());
    }

    #[test]
    fn matches_all_fields() {
        let spec = KeyboardSpecifier {
            name: Some("Magic Keyboard".to_string()),
            vendor: Some("Apple".to_string()),
            model: Some("0x05ac:0xa25a".to_string()),
            port: Some("USB".to_string()),
        };
        let kb = build_keyboard(
            "Magic Keyboard",
            "Apple",
            "0x05ac:0xa25a",
            "0x00120000",
            Some("USB"),
        );
        assert!(spec.matches(&kb));
    }

    #[test]
    fn matches_partial_fields() {
        let spec = KeyboardSpecifier {
            name: Some("magic keyboard".to_string()),
            vendor: None,
            model: None,
            port: None,
        };
        let kb = build_keyboard(
            "Magic Keyboard",
            "Apple",
            "0x05ac:0xa25a",
            "0x00120000",
            Some("USB"),
        );
        assert!(spec.matches(&kb));
    }

    #[test]
    fn matches_case_insensitive() {
        let spec = KeyboardSpecifier {
            name: None,
            vendor: Some("apple".to_string()),
            model: None,
            port: None,
        };
        let kb = build_keyboard(
            "Magic Keyboard",
            "Apple",
            "0x05ac:0xa25a",
            "0x00120000",
            Some("USB"),
        );
        assert!(spec.matches(&kb));
    }

    #[test]
    fn does_not_mismatch_name() {
        let spec = KeyboardSpecifier {
            name: Some("Logitech K845".to_string()),
            vendor: None,
            model: None,
            port: None,
        };
        let kb = build_keyboard("Magic Keyboard", "Apple", "x", "dev", None);
        assert!(!spec.matches(&kb));
    }

    #[test]
    fn port_filter_no_match_when_device_has_none() {
        // A port filter cannot match when the device has no port info.
        let spec = KeyboardSpecifier {
            name: None,
            vendor: None,
            model: None,
            port: Some("USB".to_string()),
        };
        let kb = build_keyboard("Test", "Vendor", "Model", "dev", None);
        assert!(!spec.matches(&kb));
    }

    #[test]
    fn port_filter_matches_when_device_has_port() {
        let spec = KeyboardSpecifier {
            name: None,
            vendor: None,
            model: None,
            port: Some("usb".to_string()),
        };
        let kb = build_keyboard("Test", "Vendor", "Model", "dev", Some("USB"));
        assert!(spec.matches(&kb));
    }

    #[test]
    fn serialize_roundtrip() {
        let spec = KeyboardSpecifier {
            name: Some("Magic Keyboard".to_string()),
            vendor: Some("Apple".to_string()),
            model: None,
            port: Some("Bluetooth".to_string()),
        };

        let yaml = serde_yaml::to_string(&spec).unwrap();
        let deserialized: KeyboardSpecifier =
            serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn filter_none_returns_all() {
        let keyboards = vec![
            build_keyboard("K1", "VendorA", "ModelA", "dev1", Some("USB")),
            build_keyboard("K2", "VendorB", "ModelB", "dev2", None),
        ];
        let result = filter_keyboards_by_specifiers(&keyboards, None);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_empty_slice_returns_all() {
        let keyboards = vec![
            build_keyboard("K1", "VendorA", "ModelA", "dev1", Some("USB")),
            build_keyboard("K2", "VendorB", "ModelB", "dev2", None),
        ];
        let result = filter_keyboards_by_specifiers(&keyboards, Some(&[]));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_returns_matching_only() {
        let keyboards = vec![
            build_keyboard("K1", "VendorA", "ModelA", "dev1", Some("USB")),
            build_keyboard("K2", "VendorB", "ModelB", "dev2", None),
            build_keyboard(
                "K3",
                "VendorA",
                "ModelC",
                "dev3",
                Some("Bluetooth"),
            ),
        ];
        let specs = vec![KeyboardSpecifier {
            name: None,
            vendor: Some("VendorA".to_string()),
            model: None,
            port: None,
        }];
        let result = filter_keyboards_by_specifiers(&keyboards, Some(&specs));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "K1");
        assert_eq!(result[1].name, "K3");
    }

    #[test]
    fn filter_or_logic_multiple_specifiers() {
        let keyboards = vec![
            build_keyboard("K1", "VendorA", "ModelA", "dev1", Some("USB")),
            build_keyboard("K2", "VendorB", "ModelB", "dev2", None),
            build_keyboard(
                "K3",
                "VendorC",
                "ModelC",
                "dev3",
                Some("Bluetooth"),
            ),
        ];
        let specs = vec![
            KeyboardSpecifier {
                name: None,
                vendor: Some("VendorA".to_string()),
                model: None,
                port: None,
            },
            KeyboardSpecifier {
                name: None,
                vendor: Some("VendorC".to_string()),
                model: None,
                port: None,
            },
        ];
        let result = filter_keyboards_by_specifiers(&keyboards, Some(&specs));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "K1");
        assert_eq!(result[1].name, "K3");
    }

    #[test]
    fn filter_no_matches_returns_empty() {
        let keyboards = vec![
            build_keyboard("K1", "VendorA", "ModelA", "dev1", Some("USB")),
            build_keyboard("K2", "VendorB", "ModelB", "dev2", None),
        ];
        let specs = vec![KeyboardSpecifier {
            name: None,
            vendor: Some("NonExistent".to_string()),
            model: None,
            port: None,
        }];
        let result = filter_keyboards_by_specifiers(&keyboards, Some(&specs));
        assert!(result.is_empty());
    }

    #[test]
    fn filter_empty_input_returns_empty() {
        let keyboards: Vec<KeyboardInfo> = vec![];
        let specs = vec![KeyboardSpecifier {
            name: Some("K1".to_string()),
            vendor: None,
            model: None,
            port: None,
        }];
        let result = filter_keyboards_by_specifiers(&keyboards, Some(&specs));
        assert!(result.is_empty());
    }
}
