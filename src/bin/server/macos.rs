// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::process::{Command, Stdio};

/// Check whether a process with the given name is running for the current
/// user by using `pgrep`.
pub fn is_daemon_running(name: &str) -> bool {
    Command::new("pgrep")
        .args(["-x", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Spawn the daemon as a detached background process.  Standard output and
/// error are redirected to `/dev/null` so the process is fully independent of
/// the calling terminal.
pub fn spawn_daemon(name: &str) -> Result<(), String> {
    let mut cmd = Command::new(name);
    cmd.stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());

    // Detach from the controlling process group so the daemon survives when
    // the calling shell exits.  We set a new session leader via
    // `setpgid(0, 0)` which is the standard POSIX way to background a process.
    #[cfg(target_os = "macos")]
    unsafe {
        libc::setpgid(0, 0);
    }

    cmd.spawn()
        .map_err(|e| format!("failed to start {name}: {e}"))?;

    Ok(())
}
