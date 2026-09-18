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
//! A single process-manager exposes `status` / `start` / `stop` / `restart`.
//! The platform service manager (launchd / `systemctl --user`, or a direct
//! spawn on Windows) is the sole owner of the daemon's lifecycle; all four
//! operations delegate to it through [`service`].

mod service;

/// Check whether keymapperd is running.
pub fn is_running() -> bool {
    service::is_running()
}

/// Start keymapperd.
pub fn start() -> Result<(), String> {
    service::start()
}

/// Stop keymapperd.
pub fn stop() -> Result<(), String> {
    service::stop()
}

/// Restart keymapperd.
pub fn restart() -> Result<(), String> {
    service::restart()
}

/// Check whether the macOS virtkbdd emitter is running (system domain).
#[cfg(target_os = "macos")]
pub fn virtkbdd_is_running() -> bool {
    service::virtkbdd_is_running()
}

/// Start the macOS virtkbdd emitter (system domain, through sudo).
#[cfg(target_os = "macos")]
pub fn virtkbdd_start() -> Result<(), String> {
    service::virtkbdd_start()
}

/// Stop the macOS virtkbdd emitter (system domain, through sudo).
#[cfg(target_os = "macos")]
pub fn virtkbdd_stop() -> Result<(), String> {
    service::virtkbdd_stop()
}

/// Restart the macOS virtkbdd emitter (system domain, through sudo).
#[cfg(target_os = "macos")]
pub fn virtkbdd_restart() -> Result<(), String> {
    service::virtkbdd_restart()
}
