// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Platform backend: the single public boundary between the platform
//! layer and the code above it (the daemon, `test_util`, and `cli`).
//!
//! The stable public surface that `test_util` and `cli` may depend on
//! is, per platform:
//!
//! - every platform: `list_keyboards`, `start_mapping`
//! - linux: additionally `hid_translate` (the canonical `HidUsage` and
//!   evdev-keycode tables) and `VIRTUAL_KEYBOARD_NAME`
//! - macos: additionally `KarabinerClient`, `INJECTION_KEYBOARD_IDENTITY`,
//!   `cg_keycode_to_hid_usage`
//! - windows: additionally `Key`
//!
//! Layering rule: `test_util` and `cli` may depend only on this
//! surface, never on the `pub(crate)` internals of the platform
//! module. Anything not re-exported here is private implementation
//! detail and may change without notice.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

// Only the public API surface is re-exported.  Internal helpers (signal
// handlers, static flags) stay private to the platform module.
#[cfg(target_os = "linux")]
pub use linux::hid_translate;
#[cfg(target_os = "linux")]
pub use linux::{
    VIRTUAL_KEYBOARD_NAME, discover_and_open_keyboards, list_keyboards,
    start_mapping,
};
#[cfg(target_os = "macos")]
pub use macos::{
    HidDevice, HidDeviceManager, HidQueue, HidQueueHandle, HidValueCallback,
    INJECTION_KEYBOARD_IDENTITY, IOHIDQueue, KarabinerClient,
    cg_keycode_to_hid_usage, cg_keycode_to_hid_usage_full, for_each_hid_value,
    list_keyboards, start_mapping,
};
#[cfg(target_os = "windows")]
pub use windows::CAPTURE_TAG;
#[cfg(target_os = "windows")]
pub use windows::{Key, list_keyboards, start_mapping};
