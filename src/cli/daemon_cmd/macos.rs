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

use std::process::Command;

/// The keymapperd process name (used for `pgrep` checks).
const DAEMON_NAME: &str = "keymapperd";

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

/// Build a `launchctl` command, optionally prefixed with `sudo`.
///
/// The system domain is only accessible to root, so virtkbdd operations go
/// through `sudo`.  Any sudo password prompt is read from the controlling
/// terminal, not from our stdio.
fn launchctl(sudo: bool) -> Command {
    if sudo {
        let mut cmd = Command::new("sudo");
        cmd.arg("launchctl");
        cmd
    } else {
        Command::new("launchctl")
    }
}

/// The `domain/label` target specifier for a launchd service.
///
/// The slash form is required: on recent macOS (Tahoe and later) the
/// two-argument `launchctl <verb> <domain> <label>` form fails with
/// "failed: 5: Input/output error" and leaves the service untouched, while
/// `launchctl <verb> <domain>/<label>` works.
fn target(domain: &str, label: &str) -> String {
    format!("{domain}/{label}")
}

/// Check whether a service is currently loaded (registered) with launchd in
/// the given domain.
///
/// `launchctl print <domain>/<label>` exits 0 when the service is known to
/// launchd and non-zero (with a "Could not find service" message) when it is
/// not.  This reflects the *loaded* state that `bootout` unloads, which is
/// distinct from whether the process is alive (see [`is_running`]).
fn is_loaded(sudo: bool, domain: &str, label: &str) -> bool {
    launchctl(sudo)
        .args(["print", &target(domain, label)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Boot a service out of launchd, treating the "not loaded" no-op as success.
///
/// `launchctl bootout` exits non-zero both when the service is not loaded (a
/// no-op we treat as success) and on genuine failures.  Rather than parsing
/// the version-specific message, we confirm the actual state: if the service
/// is no longer known to launchd, it has been stopped (or was already
/// stopped).
fn bootout(sudo: bool, domain: &str, label: &str) -> Result<(), String> {
    let output = launchctl(sudo)
        .args(["bootout", &target(domain, label)])
        .output()
        .map_err(|e| format!("failed to invoke launchctl: {e}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);

    // A sudo-level failure (e.g. "sudo: a password is required") means we
    // never reached launchd, so the state check below would be meaningless.
    if sudo && stderr.contains("sudo:") {
        return Err(format!(
            "launchctl bootout failed: {}",
            stderr.trim().lines().next().unwrap_or("unknown error")
        ));
    }

    if !is_loaded(sudo, domain, label) {
        return Ok(());
    }

    Err(format!(
        "launchctl bootout failed: {}",
        stderr.trim().lines().next().unwrap_or("unknown error")
    ))
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
pub fn is_running() -> bool {
    Command::new("pgrep")
        .args(["-x", DAEMON_NAME])
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
pub fn start() -> Result<(), String> {
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
    launchctl(false)
        .args(["bootout", &target(&domain, SERVICE_LABEL)])
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

    verify_daemon_started()
}

/// The unified-logging command for inspecting keymapperd's output.  The
/// daemon logs through the `log` facade, so startup failures are no longer
/// in the launchd log files.
const KEYMAPPERD_LOG_COMMAND: &str =
    "log show --predicate 'process == \"keymapperd\"' --last 5m";

/// Confirm the daemon process actually came up after a `launchctl bootstrap`.
///
/// `launchctl bootstrap` succeeds as soon as launchd accepts the job, even if
/// the process then fails to start.  With `KeepAlive` set in the plist, a
/// daemon that crashes on startup is restarted in a loop (with throttling), so
/// we poll briefly for the process to appear, then wait a short stability
/// window and confirm it is still alive.  On failure we point at the unified
/// log so the user can see why the daemon exited.
fn verify_daemon_started() -> Result<(), String> {
    // Poll for the process to appear; launchd spawns it asynchronously, so it
    // may take a moment after bootstrap returns.
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut appeared = false;
    while std::time::Instant::now() < deadline {
        if is_running() {
            appeared = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    if !appeared {
        return Err(format!(
            "{DAEMON_NAME} did not start. Check the system log: \
             {KEYMAPPERD_LOG_COMMAND}"
        ));
    }

    // Wait a short stability window and confirm it is still alive.  This
    // catches a daemon that spawns and then crashes immediately (for example
    // when the Input Monitoring / Accessibility permissions are missing or the
    // configuration is invalid).
    std::thread::sleep(std::time::Duration::from_millis(500));
    if !is_running() {
        return Err(format!(
            "{DAEMON_NAME} started but exited immediately. Check the system \
             log: {KEYMAPPERD_LOG_COMMAND}"
        ));
    }

    Ok(())
}

/// Stop the keymapperd service via launchd.
pub fn stop() -> Result<(), String> {
    bootout(false, &gui_domain(), SERVICE_LABEL)
}

/// Restart the keymapperd service via launchd.
///
/// Boots the service out and back in, then confirms the daemon process
/// actually came up (see [`verify_daemon_started`]) — `launchctl bootstrap`
/// alone does not guarantee the process started.
pub fn restart() -> Result<(), String> {
    stop()?;
    // Brief pause to let launchd fully clean up the old process.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let plist = plist_path();
    let domain = gui_domain();
    Command::new("launchctl")
        .args(["bootstrap", &domain, &plist])
        .output()
        .map_err(|e| format!("failed to invoke launchctl: {e}"))?;

    verify_daemon_started()
}

/// Check whether the virtkbdd process is actually running (system domain).
///
/// As with [`is_running`], the authoritative check is `pgrep -x virtkbdd`,
/// which reports whether a live process with that exact name exists.  `sudo
/// launchctl print system <label>` only tells us the service is *known* to
/// launchd, which is true even for a loaded service whose process has crashed
/// or exited.  `pgrep` sees the root-owned virtkbdd process even when run as
/// an unprivileged user, so no `sudo` is needed for this check.
pub fn virtkbdd_is_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "virtkbdd"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Start the virtkbdd service via launchd (system domain, through sudo).
pub fn virtkbdd_start() -> Result<(), String> {
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
    launchctl(true)
        .args(["bootout", &target("system", VIRTKBDD_LABEL)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok(); // Ignore — service may not be loaded yet.

    let output = launchctl(true)
        .args(["bootstrap", "system", &plist])
        .output()
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
pub fn virtkbdd_stop() -> Result<(), String> {
    bootout(true, "system", VIRTKBDD_LABEL)
}

/// Restart the virtkbdd service via launchd (system domain, through sudo).
pub fn virtkbdd_restart() -> Result<(), String> {
    virtkbdd_stop()?;
    // Brief pause to let launchd fully clean up the old process.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let plist = virtkbdd_plist_path();
    let output = launchctl(true)
        .args(["bootstrap", "system", &plist])
        .output()
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
