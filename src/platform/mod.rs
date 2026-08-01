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
pub(crate) use linux::find_keyboard_device;
#[cfg(target_os = "linux")]
pub use linux::{Key, list_keyboards, start_mapping};
#[cfg(target_os = "macos")]
pub use macos::{Key, list_keyboards, start_mapping};
#[cfg(target_os = "windows")]
pub use windows::{Key, list_keyboards, start_mapping};
