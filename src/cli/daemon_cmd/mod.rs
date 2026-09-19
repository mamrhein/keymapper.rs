// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Daemon process management for keymapperd.
//!
//! The platform service manager (launchd / `systemctl --user`, or a direct
//! spawn on Windows) is the sole owner of the daemon's lifecycle; all
//! operations are provided by the platform-specific modules.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::{is_running, restart, start, stop};
#[cfg(target_os = "macos")]
pub use macos::{
    is_running, restart, start, stop, virtkbdd_is_running, virtkbdd_restart,
    virtkbdd_start, virtkbdd_stop,
};
#[cfg(target_os = "windows")]
pub use windows::{is_running, restart, start, stop};
