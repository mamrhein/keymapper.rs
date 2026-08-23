// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

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
pub(crate) use linux::{
    VIRTUAL_KEYBOARD_NAME, get_active_app_name,
    hid_translate::keycode_to_hid_usage,
};
#[cfg(target_os = "linux")]
pub use linux::{discover_and_open_keyboards, list_keyboards, start_mapping};
#[cfg(target_os = "macos")]
pub(crate) use macos::get_active_app_name;
#[cfg(target_os = "macos")]
pub use macos::{HidVirtKbdConn, list_keyboards, start_mapping};
#[cfg(target_os = "windows")]
pub use windows::CAPTURE_TAG;
#[cfg(target_os = "windows")]
pub(crate) use windows::get_active_app_name;
#[cfg(target_os = "windows")]
pub use windows::{Key, list_keyboards, start_mapping};
