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

/// Check whether a keymapperd process is actually running.
///
/// The authoritative check is `pgrep -x <name>`, which reports whether a live
/// process with that exact name exists.  We deliberately do **not** rely on
/// `launchctl print gui/<UID> <label>` for this: that command succeeds (exit
/// code 0) whenever the service is merely *known* to launchd, which is true
/// even for a loaded service whose process has crashed or exited (for example
/// when `KeepAlive` is false).  Relying on it alone would report a dead daemon
/// as running.  `pgrep` also covers the case where the daemon was started
/// manually rather than through launchd.
pub fn is_daemon_running(name: &str) -> bool {
    Command::new("pgrep")
        .args(["-x", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Start the keymapperd service via launchd.
///
/// Boots the service using `launchctl bootstrap gui/<UID> <plist>` and then
/// confirms the daemon process actually came up.  `launchctl bootstrap`
/// returns success as soon as launchd accepts the job, which does not by
/// itself guarantee the process started — with `KeepAlive` set in the plist,
/// a daemon that crashes on startup is restarted in a loop — so we verify the
/// process is genuinely alive before reporting success.
pub fn spawn_daemon(name: &str) -> Result<(), String> {
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

    verify_daemon_started(name)
}

/// The path to the keymapperd error log written by launchd.
fn keymapperd_log_path() -> String {
    format!(
        "{}/Library/Logs/keymapper/keymapperd-err.log",
        std::env::var("HOME").unwrap_or_default()
    )
}

/// Confirm the daemon process actually came up after a `launchctl bootstrap`.
///
/// `launchctl bootstrap` succeeds as soon as launchd accepts the job, even if
/// the process then fails to start.  With `KeepAlive` set in the plist, a
/// daemon that crashes on startup is restarted in a loop (with throttling), so
/// we poll briefly for the process to appear, then wait a short stability
/// window and confirm it is still alive.  On failure we point at the log file
/// so the user can see why the daemon exited.
fn verify_daemon_started(name: &str) -> Result<(), String> {
    let log = keymapperd_log_path();

    // Poll for the process to appear; launchd spawns it asynchronously, so it
    // may take a moment after bootstrap returns.
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut appeared = false;
    while std::time::Instant::now() < deadline {
        if is_daemon_running(name) {
            appeared = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    if !appeared {
        return Err(format!("{name} did not start. Check the log at {log}"));
    }

    // Wait a short stability window and confirm it is still alive.  This
    // catches a daemon that spawns and then crashes immediately (for example
    // when the Input Monitoring / Accessibility permissions are missing or the
    // configuration is invalid).
    std::thread::sleep(std::time::Duration::from_millis(500));
    if !is_daemon_running(name) {
        return Err(format!(
            "{name} started but exited immediately. Check the log at {log}"
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
///
/// Boots the service out and back in, then confirms the daemon process
/// actually came up (see [`verify_daemon_started`]) — `launchctl bootstrap`
/// alone does not guarantee the process started.
pub fn restart_daemon(name: &str) -> Result<(), String> {
    stop_daemon()?;
    // Brief pause to let launchd fully clean up the old process.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let plist = plist_path();
    let domain = gui_domain();
    Command::new("launchctl")
        .args(["bootstrap", &domain, &plist])
        .output()
        .map_err(|e| format!("failed to invoke launchctl: {e}"))?;

    verify_daemon_started(name)
}

/// Check whether the virtkbdd process is actually running (system domain).
///
/// As with [`is_daemon_running`], the authoritative check is `pgrep -x
/// virtkbdd`, which reports whether a live process with that exact name
/// exists.  `sudo launchctl print system <label>` only tells us the service is
/// *known* to launchd, which is true even for a loaded service whose process
/// has crashed or exited.  `pgrep` sees the root-owned virtkbdd process even
/// when run as an unprivileged user, so no `sudo` is needed for this check.
pub fn is_virtkbdd_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "virtkbdd"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
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
