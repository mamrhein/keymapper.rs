// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Platform-agnostic keyboard device metadata.

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
