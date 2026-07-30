// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS launchd integration for managing the keymapperd daemon.
//!
//! Uses `launchctl` to query, start, stop and restart the user-level launchd
//! service.  Requires the plist to be installed at
//! `~/Library/LaunchAgents/de.adrhinum.keymapperd.plist` (done by the
//! install script).

use std::process::Command;

/// The launchd label used to identify the keymapperd service.
const SERVICE_LABEL: &str = "de.adrhinum.keymapperd";

/// The launchd domain for the current user's graphical session.
///
/// `gui/<UID>` is the standard domain for per-user agents on macOS.  It has
/// been stable since macOS 10.10 (Yosemite).
fn gui_domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

/// The path to the plist file in the user's LaunchAgents directory.
fn plist_path() -> String {
    format!(
        "{}/Library/LaunchAgents/{}.plist",
        std::env::var("HOME").unwrap_or_default(),
        SERVICE_LABEL
    )
}

/// Check whether the keymapperd launchd service is loaded and running.
///
/// `launchctl print gui/<UID> <label>` succeeds (exit code 0) when the
/// service is known to launchd.  A loaded service that has crashed will still
/// be reported as known, so we also check `pgrep` as a fallback to confirm
/// the process is alive.
pub fn is_daemon_running(_name: &str) -> bool {
    // Check if launchd knows about the service.
    let print_ok = Command::new("launchctl")
        .args(["print", &gui_domain(), SERVICE_LABEL])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if print_ok {
        return true;
    }

    // Fallback: check if the process is running via pgrep.  This covers the
    // case where the service was started manually (not via launchd).
    Command::new("pgrep")
        .args(["-x", "keymapperd"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Start the keymapperd service via launchd.
///
/// Boots the service using `launchctl bootstrap gui/<UID> <plist>`.  This is
/// a synchronous call — launchd returns once the service has been started (or
/// failed to start).
pub fn spawn_daemon(_name: &str) -> Result<(), String> {
    let plist = plist_path();

    // Verify the plist exists before attempting to boot it.
    if !std::path::Path::new(&plist).exists() {
        return Err(format!(
            "launchd plist not found at {}. Install the service first: \
             scripts/install-macos.sh",
            plist
        ));
    }

    // If the service is already loaded, boot it out first to ensure a clean
    // start.  This makes `start` idempotent and doubles as a restart.
    let domain = gui_domain();
    Command::new("launchctl")
        .args(["bootout", &domain, SERVICE_LABEL])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok(); // Ignore — service may not be loaded yet.

    let output = Command::new("launchctl")
        .args(["bootstrap", &domain, &plist])
        .output()
        .map_err(|e| format!("failed to invoke launchctl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "launchctl bootstrap failed: {}",
            stderr.trim().lines().next().unwrap_or("unknown error")
        ));
    }

    Ok(())
}

/// Stop the keymapperd service via launchd.
pub fn stop_daemon() -> Result<(), String> {
    let output = Command::new("launchctl")
        .args(["bootout", &gui_domain(), SERVICE_LABEL])
        .output()
        .map_err(|e| format!("failed to invoke launchctl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // bootout returns non-zero if the service is not loaded, which is a
        // no-op condition we treat as success.
        if stderr.contains("does not exist") || stderr.contains("not found") {
            return Ok(());
        }
        return Err(format!(
            "launchctl bootout failed: {}",
            stderr.trim().lines().next().unwrap_or("unknown error")
        ));
    }

    Ok(())
}

/// Restart the keymapperd service via launchd.
pub fn restart_daemon() -> Result<(), String> {
    stop_daemon()?;
    // Brief pause to let launchd fully clean up the old process.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let plist = plist_path();
    let domain = gui_domain();
    Command::new("launchctl")
        .args(["bootstrap", &domain, &plist])
        .output()
        .map_err(|e| format!("failed to invoke launchctl: {e}"))?;

    Ok(())
}
