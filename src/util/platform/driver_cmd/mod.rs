// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Cross-platform driver management commands.
//!
//! On macOS this provides `install` and `status` for the DriverKit virtual
//! HID keyboard.  On other platforms it is a no-op, since the driver only
//! exists on macOS.

#[cfg(target_os = "macos")]
mod macos;

/// Result of a [`status`] query.  Captures the full state of the virtual HID
/// driver so the CLI can print a human-readable summary.
#[derive(Debug, Default)]
pub struct DriverStatus {
    /// Whether the `.kext` bundle exists on disk (at either install
    /// location).
    pub installed: bool,
    /// The resolved path to the `.kext` bundle, if found.
    pub installed_path: Option<std::path::PathBuf>,
    /// Whether the driver is loaded and visible in the IOKit registry.
    pub loaded_in_iokit: bool,
    /// Whether a socket connection to the driver can be established.
    pub socket_connected: bool,
    /// Always `"ad-hoc"` for this project.
    pub signing: String,
}

/// Build the DriverKit extension from source and copy it to the local
/// install location.
///
/// This is the implementation of `keymapper driver install`.  On macOS it
/// verifies that `xcodebuild` is available, builds the driver with ad-hoc
/// signing, and copies the resulting `.kext` to
/// `~/Library/Application Support/keymapper/`.
///
/// On non-macOS platforms this returns `Ok(())` without doing anything.
#[cfg(target_os = "macos")]
pub fn install() -> Result<(), String> {
    macos::install()
}

#[cfg(not(target_os = "macos"))]
pub fn install() -> Result<(), String> {
    Ok(())
}

/// Query the current state of the virtual HID driver and return a
/// [`DriverStatus`] summary.
///
/// Checks both known install locations for the `.kext` bundle, queries IOKit
/// for a matching service, and attempts to open an `IOHIDServiceSocket`.
///
/// On non-macOS platforms returns a default status with all fields `false`
/// and no path.
#[cfg(target_os = "macos")]
pub fn status() -> DriverStatus {
    macos::status()
}

#[cfg(not(target_os = "macos"))]
pub fn status() -> DriverStatus {
    DriverStatus::default()
}
