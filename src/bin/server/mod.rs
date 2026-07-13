// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Platform-specific helpers for managing the keymapperd daemon process.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

const DAEMON_NAME: &str = "keymapperd";

/// Check whether a keymapperd process is running for the current user.
#[cfg(target_os = "macos")]
pub fn is_running() -> bool {
    macos::is_daemon_running(DAEMON_NAME)
}

#[cfg(target_os = "linux")]
pub fn is_running() -> bool {
    linux::is_daemon_running(DAEMON_NAME)
}

#[cfg(target_os = "windows")]
pub fn is_running() -> bool {
    windows::is_daemon_running(DAEMON_NAME)
}

/// Attempt to start keymapperd as a background process.
///
/// Returns `Ok(())` when the process was spawned successfully, regardless of
/// whether it stays alive.  Returns `Err` only if the executable could not be
/// found or the spawn itself failed.
#[cfg(target_os = "macos")]
pub fn start() -> Result<(), String> {
    macos::spawn_daemon(DAEMON_NAME)
}

#[cfg(target_os = "linux")]
pub fn start() -> Result<(), String> {
    linux::spawn_daemon(DAEMON_NAME)
}

#[cfg(target_os = "windows")]
pub fn start() -> Result<(), String> {
    windows::spawn_daemon(DAEMON_NAME)
}
