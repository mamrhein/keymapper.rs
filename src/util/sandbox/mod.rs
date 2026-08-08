// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Platform-abstracted sandbox for end-to-end testing.
//!
//! Provides a uniform interface to inject synthetic keyboard events and
//! capture the daemon's output, hiding the per-platform details of virtual
//! device creation, event injection, and event monitoring.

use std::fmt;

/// A single captured keyboard event.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct CapturedEvent {
    /// The platform-native keycode (CGKeyCode on macOS, evdev code on Linux,
    /// VIRTUAL_KEY on Windows).
    pub code: u16,
    /// `true` for key-down, `false` for key-up.
    pub is_down: bool,
}

/// Errors that can occur during sandbox setup or operation.
#[allow(dead_code)]
#[derive(Debug)]
pub enum SandboxError {
    /// Accessibility permission is missing (macOS) or equivalent privilege
    /// is not granted on another platform.
    PermissionDenied(String),

    /// Failed to create a virtual keyboard or event monitor.
    DeviceCreationFailed(String),

    /// Failed to inject an event into the virtual input device.
    InjectionFailed(String),

    /// The sandbox is not supported on this platform in its current
    /// configuration.
    NotSupported(String),
}

impl fmt::Display for SandboxError {
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

impl std::error::Error for SandboxError {}

/// Platform-abstracted sandbox for end-to-end keyboard mapping tests.
///
/// The lifecycle is:
/// 1. Create a sandbox instance (`new()`).
/// 2. Call `setup()` to create virtual devices and start monitoring.
/// 3. Inject events with `inject_key_down()` / `inject_key_up()`.
/// 4. Read output with `drain_output_events()`.
/// 5. Call `teardown()` to clean up (also called on `Drop`).
///
/// Note: This trait does not require `Send + Sync` because platform
/// implementations use internal types (e.g. `CFRetained<CGEventSource>`
/// on macOS) that do not implement these bounds.  Cross-thread state
/// sharing is handled via `Arc<Mutex<..>>` wrappers on a per-field basis.
#[allow(dead_code)]
pub trait Sandbox {
    /// Create a new sandbox instance.
    ///
    /// Returns `None` if the platform is fundamentally unsupported (e.g.
    /// missing compile-time features).  Returns an `Err` if the platform is
    /// supported but runtime prerequisites are not met (e.g. missing
    /// permissions).
    fn new() -> Result<Option<Self>, SandboxError>
    where
        Self: Sized;

    /// Finalize setup: start the event monitor, create virtual devices, etc.
    ///
    /// After this call the sandbox is ready to inject and capture events.
    fn setup(&mut self) -> Result<(), SandboxError>;

    /// Inject a key-down event into the virtual input keyboard.
    fn inject_key_down(&self, code: u16) -> Result<(), SandboxError>;

    /// Inject a key-up event into the virtual input keyboard.
    fn inject_key_up(&self, code: u16) -> Result<(), SandboxError>;

    /// Drain and return all output events captured since the last call.
    ///
    /// "Output" means events that reach the session level — this includes
    /// both unmapped passthrough keys and the daemon's remapped output.
    /// Events that are swallowed by the daemon (mapped triggers) do not
    /// appear here.
    fn drain_output_events(&self) -> Vec<CapturedEvent>;

    /// Return the platform-specific identifier of the virtual input keyboard,
    /// if one exists.
    ///
    /// On Linux this is the device node path (e.g. `/dev/input/event4`).
    /// On macOS and Windows this returns `None` because the event taps and
    /// hooks are global and do not target a specific device.
    fn input_device_id(&self) -> Option<&str>;

    /// Shut down the sandbox and release all resources.
    ///
    /// This is also called automatically on `Drop` of the concrete
    /// implementation, but explicit teardown allows tests to check for
    /// cleanup errors.
    fn teardown(&mut self);
}

// ---------------------------------------------------------------------------
// Platform-specific implementations
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::MacoSandbox;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::LinuxSandbox;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
#[allow(unused_imports)]
pub use windows::WindowsSandbox;
