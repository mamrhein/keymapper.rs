// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Service-manager process management for keymapperd (production mode).
//!
//! On macOS and Linux this delegates to the native service manager (launchd /
//! `systemctl --user`).  On Windows it directly spawns the daemon binary.
//! This is the backend selected when no `--config-dir` is provided; the
//! PID-file (development) backend lives in [`super::pid_file`].

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

/// Attempt to start keymapperd via the platform service manager (or direct
/// spawn on Windows).
///
/// Returns `Ok(())` when the daemon was started successfully.  On macOS and
/// Linux the service manager handles this synchronously, so no additional
/// verification is needed.
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
    let spawn_result = windows::spawn_daemon(DAEMON_NAME);
    verify_start(spawn_result)
}

/// Stop the keymapperd service.
#[cfg(target_os = "macos")]
pub fn stop() -> Result<(), String> {
    macos::stop_daemon()
}

#[cfg(target_os = "linux")]
pub fn stop() -> Result<(), String> {
    linux::stop_daemon()
}

#[cfg(target_os = "windows")]
pub fn stop() -> Result<(), String> {
    // On Windows we send a termination signal by finding the process and
    // closing its handle.  For now fall back to asking the user to use the
    // Task Manager or a future Windows service implementation.
    Err(
        "stop is not supported on Windows yet; use Task Manager or restart \
         to stop keymapperd"
            .into(),
    )
}

/// Restart the keymapperd service.
#[cfg(target_os = "macos")]
pub fn restart() -> Result<(), String> {
    macos::restart_daemon()
}

#[cfg(target_os = "linux")]
pub fn restart() -> Result<(), String> {
    linux::restart_daemon()
}

#[cfg(target_os = "windows")]
pub fn restart() -> Result<(), String> {
    stop()?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    start()
}

/// After a successful spawn, wait briefly and confirm the daemon is still
/// alive.
///
/// Only used on Windows where we spawn directly and need to verify the process
/// didn't crash immediately.
#[cfg(target_os = "windows")]
fn verify_start(spawn_result: Result<(), String>) -> Result<(), String> {
    spawn_result?;

    // Give the daemon time to initialize or fail.
    std::thread::sleep(std::time::Duration::from_millis(500));

    if !is_running() {
        return Err("daemon started but exited immediately".to_string());
    }

    Ok(())
}
