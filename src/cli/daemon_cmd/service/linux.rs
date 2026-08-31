// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux systemd integration for managing the keymapperd daemon.
//!
//! Uses `systemctl --user` to query, start, stop and restart the user-level
//! systemd service.  Requires the unit file to be installed at
//! `~/.config/systemd/user/keymapperd.service` (done by the install script).

use std::process::Command;

/// The systemd user unit name.
const SERVICE_NAME: &str = "keymapperd.service";

/// Helper that runs `systemctl --user` with the given arguments.
fn systemctl(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("systemctl")
        .args(["--user"])
        .args(args)
        .output()
        .map_err(|e| format!("failed to invoke systemctl: {e}"))
}

/// Check whether the keymapperd systemd user service is active.
pub fn is_daemon_running(_name: &str) -> bool {
    systemctl(&["is-active", "main", SERVICE_NAME])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Start the keymapperd service via systemd.
pub fn spawn_daemon(_name: &str) -> Result<(), String> {
    // Check that the unit file is installed.
    let unit_path =
        std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".config/systemd/user/")
            .join(SERVICE_NAME);

    if !unit_path.exists() {
        return Err(format!(
            "systemd unit not found at {}. Install the service first: \
             scripts/install-linux.sh",
            unit_path.display()
        ));
    }

    let output = systemctl(&["start", SERVICE_NAME])?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "systemctl start failed: {}",
            stderr.trim().lines().next().unwrap_or("unknown error")
        ));
    }

    Ok(())
}

/// Stop the keymapperd service via systemd.
pub fn stop_daemon() -> Result<(), String> {
    let output = systemctl(&["stop", SERVICE_NAME])?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "systemctl stop failed: {}",
            stderr.trim().lines().next().unwrap_or("unknown error")
        ));
    }

    Ok(())
}

/// Restart the keymapperd service via systemd.
pub fn restart_daemon() -> Result<(), String> {
    let output = systemctl(&["restart", SERVICE_NAME])?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "systemctl restart failed: {}",
            stderr.trim().lines().next().unwrap_or("unknown error")
        ));
    }

    Ok(())
}
