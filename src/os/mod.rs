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
// handlers, static flags, device discovery) stay private to the platform
// module.

#[cfg(target_os = "linux")]
pub use linux::{Key, start_mapping};
#[cfg(target_os = "macos")]
pub use macos::{Key, start_mapping};
#[cfg(target_os = "windows")]
pub use windows::{Key, start_mapping};
