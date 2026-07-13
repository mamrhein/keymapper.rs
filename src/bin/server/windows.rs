// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::process::{Command, Stdio};

/// Check whether a process with the given name is running by using
/// `tasklist`.
pub fn is_daemon_running(name: &str) -> bool {
    // Append ".exe" if not already present — tasklist requires the full name.
    let image = if name.ends_with(".exe") {
        name.to_string()
    } else {
        format!("{name}.exe")
    };

    let Ok(output) = Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {image}")])
        .output()
    else {
        return false;
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.contains(&image)
}

/// Spawn the daemon as a background process without creating a console window.
pub fn spawn_daemon(name: &str) -> Result<(), String> {
    // On Windows the standard way to hide a console window is using
    // CREATE_NO_WINDOW (0x08000000).  std::process::Command doesn't expose
    // this directly, so we wrap the command in `start /B` which achieves a
    // similar effect.
    let output = Command::new("cmd")
        .args(&["/C", "start", "", "/B", name])
        .spawn()
        .map_err(|e| format!("failed to start {name}: {e}"))?;

    // The `start` command returns immediately; the actual daemon is detached.
    drop(output);

    Ok(())
}
