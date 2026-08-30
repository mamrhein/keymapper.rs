// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Platform-abstracted keyboard injector for end-to-end testing.
//!
//! Provides a uniform interface to inject synthetic keyboard events,
//! hiding the per-platform details of virtual device creation and event
//! injection.  Capture is handled by a separate monitor binary that
//! writes events to a log file.

use std::fmt;

/// Errors that can occur during injector setup or operation.
#[allow(dead_code)]
#[derive(Debug)]
pub enum InjectorError {
    /// Accessibility permission is missing (macOS) or equivalent privilege
    /// is not granted on another platform.
    PermissionDenied(String),

    /// Failed to create a virtual keyboard or event source.
    DeviceCreationFailed(String),

    /// Failed to inject an event into the virtual input device.
    InjectionFailed(String),

    /// The injector is not supported on this platform in its current
    /// configuration.
    NotSupported(String),
}

impl fmt::Display for InjectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PermissionDenied(msg) => {
                write!(f, "permission denied: {msg}")
            }
            Self::DeviceCreationFailed(msg) => {
                write!(f, "device creation failed: {msg}")
            }
            Self::InjectionFailed(msg) => write!(f, "injection failed: {msg}"),
            Self::NotSupported(msg) => write!(f, "not supported: {msg}"),
        }
    }
}

impl std::error::Error for InjectorError {}

/// Platform-abstracted keyboard injector for end-to-end tests.
///
/// The lifecycle is:
/// 1. Create an injector instance (`new()`).
/// 2. Call `setup()` to create virtual devices.
/// 3. Inject events with `inject_key_down()` / `inject_key_up()`.
/// 4. Call `teardown()` to clean up (also called on `Drop`).
///
/// Output capture is handled externally by the monitor binary, which
/// logs events to a file for deterministic validation.
#[allow(dead_code)]
pub trait KeyInjector {
    /// Create a new injector instance.
    ///
    /// Returns `None` if the platform is fundamentally unsupported (e.g.
    /// missing compile-time features).  Returns an `Err` if the platform is
    /// supported but runtime prerequisites are not met (e.g. missing
    /// permissions).
    fn new() -> Result<Option<Self>, InjectorError>
    where
        Self: Sized;

    /// Finalize setup: create virtual devices, etc.
    ///
    /// After this call the injector is ready to inject events.
    fn setup(&mut self) -> Result<(), InjectorError>;

    /// Inject a key-down event into the virtual input keyboard.
    fn inject_key_down(&self, code: u16) -> Result<(), InjectorError>;

    /// Inject a key-up event into the virtual input keyboard.
    fn inject_key_up(&self, code: u16) -> Result<(), InjectorError>;

    /// Shut down the injector and release all resources.
    ///
    /// This is also called automatically on `Drop` of the concrete
    /// implementation, but explicit teardown allows tests to check for
    /// cleanup errors.
    fn teardown(&mut self);

    /// Path of the virtual input device node (e.g. `/dev/input/event3`),
    /// available after a successful `setup()` on platforms that inject via
    /// a device node.  Returns `None` on platforms without a device-node
    /// based injection mechanism (e.g. macOS, Windows).
    fn input_device_path(&self) -> Option<&str> {
        None
    }
}

// ---------------------------------------------------------------------------
// Platform-specific implementations
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::MacOSInjector;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::LinuxInjector;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
#[allow(unused_imports)]
pub use windows::WindowsInjector;
