// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS launchd integration for managing the keymapperd and virtkbdd
//! daemons.
//!
//! Uses `launchctl` to query, start, stop and restart both services:
//!
//! * keymapperd — a user-level LaunchAgent in `gui/<UID>`, requiring the plist
//!   at `~/Library/LaunchAgents/de.adrhinum.keymapperd.plist` (done by the
//!   install script).
//! * virtkbdd — a root LaunchDaemon in the `system` domain, requiring the
//!   plist at `/Library/LaunchDaemons/de.adrhinum.virtkbdd.plist`.  The system
//!   domain is only accessible to root, so these operations go through `sudo
//!   launchctl`.

use std::process::{Command, Output};

/// The launchd label used to identify the keymapperd service.
const SERVICE_LABEL: &str = "de.adrhinum.keymapperd";

/// The launchd label used to identify the virtkbdd service.
const VIRTKBDD_LABEL: &str = "de.adrhinum.virtkbdd";

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

/// The path to the virtkbdd plist in the system LaunchDaemons directory.
fn virtkbdd_plist_path() -> String {
    format!("/Library/LaunchDaemons/{VIRTKBDD_LABEL}.plist")
}

/// Run a `launchctl` subcommand in the system domain via `sudo`.
///
/// The system domain is only accessible to root.  Any sudo password prompt
/// is read from the controlling terminal, not from our stdio.
fn sudo_launchctl(args: &[&str]) -> std::io::Result<Output> {
    Command::new("sudo").arg("launchctl").args(args).output()
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

/// Check whether the virtkbdd launchd service is loaded (system domain).
pub fn is_virtkbdd_running() -> bool {
    sudo_launchctl(&["print", "system", VIRTKBDD_LABEL])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Start the virtkbdd service via launchd (system domain, through sudo).
pub fn spawn_virtkbdd() -> Result<(), String> {
    let plist = virtkbdd_plist_path();

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
    sudo_launchctl(&["bootout", "system", VIRTKBDD_LABEL]).ok();

    let output = sudo_launchctl(&["bootstrap", "system", &plist])
        .map_err(|e| format!("failed to invoke sudo launchctl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "launchctl bootstrap failed: {}",
            stderr.trim().lines().next().unwrap_or("unknown error")
        ));
    }

    Ok(())
}

/// Stop the virtkbdd service via launchd (system domain, through sudo).
pub fn stop_virtkbdd() -> Result<(), String> {
    let output = sudo_launchctl(&["bootout", "system", VIRTKBDD_LABEL])
        .map_err(|e| format!("failed to invoke sudo launchctl: {e}"))?;

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

/// Restart the virtkbdd service via launchd (system domain, through sudo).
pub fn restart_virtkbdd() -> Result<(), String> {
    stop_virtkbdd()?;
    // Brief pause to let launchd fully clean up the old process.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let plist = virtkbdd_plist_path();
    let output = sudo_launchctl(&["bootstrap", "system", &plist])
        .map_err(|e| format!("failed to invoke sudo launchctl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "launchctl bootstrap failed: {}",
            stderr.trim().lines().next().unwrap_or("unknown error")
        ));
    }

    Ok(())
}
