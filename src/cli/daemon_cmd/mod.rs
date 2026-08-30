// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! PID-based process management for keymapperd.
//!
//! Spawns the daemon as a background child process and tracks it via a PID
//! file.  This is used for development / e2e testing when `--config-dir` is
//! provided.  Production (service-manager) mode is handled by `server_cmd`.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use linux::{spawn_daemon, terminate_daemon};
#[cfg(target_os = "macos")]
use macos::{spawn_daemon, terminate_daemon};
#[cfg(target_os = "windows")]
use windows::{spawn_daemon, terminate_daemon};

/// The default cache directory name used to store the PID file.
const CACHE_DIR: &str = "keymapper";

/// The PID file name.
const PID_FILE: &str = "keymapperd.pid";

/// Determine where the PID file should be stored.
///
/// When `config_dir` is provided, places it inside that directory.  Otherwise
/// falls back to the platform cache directory (`~/.cache/keymapper/`).
fn pid_file_path(config_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = config_dir {
        return dir.join(PID_FILE);
    }

    // Fallback to user cache directory.
    if let Some(home) = dirs::home_dir() {
        let cache = home.join(".cache").join(CACHE_DIR);
        let _ = fs_err::create_dir_all(&cache);
        return cache.join(PID_FILE);
    }

    // Last resort: current directory.
    PathBuf::from(PID_FILE)
}

/// Read the PID from the given file, returning `None` if the file doesn't
/// exist or can't be parsed.
fn read_pid(path: &Path) -> Option<u32> {
    let content = fs_err::read_to_string(path).ok()?;
    content.trim().parse::<u32>().ok()
}

/// Check whether the process with the given PID is alive.
#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    // kill(pid, 0) returns ESRCH when the process doesn't exist.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(target_os = "windows")]
fn is_pid_alive(pid: u32) -> bool {
    windows::is_process_alive(pid)
}

/// Check whether the daemon is running by reading the PID file and verifying
/// the process exists.
pub fn is_running(config_dir: Option<&Path>) -> bool {
    let pid_path = pid_file_path(config_dir);
    if let Some(pid) = read_pid(&pid_path) {
        return is_pid_alive(pid);
    }
    false
}

/// Start keymapperd as a background process with `current_dir` set to the
/// given config directory.  The PID is written to a PID file so that it can
/// be stopped later.
pub fn start(config_dir: &Path) -> Result<(), String> {
    if is_running(Some(config_dir)) {
        return Err(
            "daemon is already running (managed by --config-dir)".into()
        );
    }

    let pid_path = pid_file_path(Some(config_dir));

    // Ensure the parent directory of the PID file exists.
    if let Some(parent) = pid_path.parent() {
        fs_err::create_dir_all(parent).map_err(|e| {
            format!("failed to create PID file directory: {e}")
        })?;
    }

    let (child_pid, error) = spawn_daemon(config_dir)?;

    // Persist the PID so we can stop it later.
    fs_err::write(&pid_path, child_pid.to_string())
        .map_err(|e| format!("failed to write PID file: {e}"))?;

    // Brief grace period for the daemon to initialize or fail fast.
    std::thread::sleep(std::time::Duration::from_millis(100));

    if !is_pid_alive(child_pid) {
        // Clean up the stale PID file.
        let _ = fs_err::remove_file(&pid_path);
        return Err(
            error.unwrap_or_else(|| "daemon exited immediately".into())
        );
    }

    Ok(())
}

/// Stop the daemon by reading the PID file and sending a termination signal.
///
/// Sends SIGTERM first, waits up to 5 seconds, then escalates to SIGKILL.
/// On Windows, uses `TerminateProcess` directly.
pub fn stop(config_dir: Option<&Path>) -> Result<(), String> {
    let pid_path = pid_file_path(config_dir);

    let Some(pid) = read_pid(&pid_path) else {
        return Err("daemon is not running (no PID file found)".into());
    };

    if !is_pid_alive(pid) {
        // Stale PID file — clean up and report that the daemon isn't running.
        let _ = fs_err::remove_file(&pid_path);
        return Err("daemon is not running (stale PID file)".into());
    }

    terminate_daemon(pid)?;

    // Wait for the process to actually exit.
    let mut waited = 0;
    while is_pid_alive(pid) && waited < 50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        waited += 1;
    }

    // Clean up the PID file.
    let _ = fs_err::remove_file(&pid_path);

    Ok(())
}

/// Restart the daemon in the given config directory.
pub fn restart(config_dir: &Path) -> Result<(), String> {
    stop(Some(config_dir))?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    start(config_dir)
}
