// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Visible application list on Linux.
//!
//! Scans `/proc` for processes connected to a display server (via environment
//! variables `WAYLAND_DISPLAY` or `DISPLAY`), resolves their executable name
//! against `.desktop` files, and returns the resulting application ids.
//!
//! This approach is independent of the display server (X11, Wayland, etc.) and
//! works uniformly across all compositors.

use std::fs;

use sysinfo::{Pid, ProcessesToUpdate, System};

/// Check if a process is connected to a display server by inspecting its
/// environment variables in `/proc/[pid]/environ`.
fn is_gui_process(pid: Pid) -> bool {
    let path = format!("/proc/{}/environ", pid.as_u32());
    let Ok(data) = fs::read(&path) else {
        return false;
    };

    // environ is null-separated KEY=VALUE pairs.  We check for the presence
    // of display server environment variables by searching for the key prefix.
    data.windows(16).any(|w| w == b"WAYLAND_DISPLAY=")
        || data.windows(8).any(|w| w == b"DISPLAY=")
}

/// Resolve the executable name for a process by reading the `/proc/[pid]/exe`
/// symlink, falling back to `/proc/[pid]/cmdline` first token.
fn resolve_exe_name(pid: Pid) -> Option<String> {
    let pid_str = pid.as_u32().to_string();

    // Try resolving the exe symlink — gives us the real binary path.
    if let Some(stem) = fs::read_link(format!("/proc/{}/exe", pid_str))
        .ok()
        .and_then(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
    {
        return Some(stem);
    }

    // Fall back to the first token of cmdline.
    let Ok(cmdline) = fs::read(format!("/proc/{}/cmdline", pid_str)) else {
        return None;
    };

    // cmdline is null-separated; first token is the executable.
    if let Some(null_pos) = cmdline.iter().position(|&b| b == 0) {
        let first = &cmdline[..null_pos];
        return String::from_utf8_lossy(first)
            .as_ref()
            .rsplit('/')
            .next()
            .map(|s| s.to_string());
    }

    // No null byte — treat whole content as the executable path.
    let cmd = String::from_utf8_lossy(&cmdline);
    if !cmd.is_empty() {
        return cmd.as_ref().rsplit('/').next().map(|s| s.to_string());
    }

    None
}

/// Return the sorted, deduplicated list of application ids for all GUI
/// processes connected to a display server.
///
/// These are the `.desktop` file stems (e.g., `"org.mozilla.firefox"`) that
/// should be used in the `apps` field of the keymapperd configuration.
pub fn list_app_names() -> Vec<String> {
    let mut system = System::new_all();
    system.refresh_processes(ProcessesToUpdate::All, false);

    let mut app_ids: Vec<String> = Vec::new();

    for process in system.processes().values() {
        // Filter to GUI processes.
        if !is_gui_process(process.pid()) {
            continue;
        }

        // Resolve the executable name and match against .desktop entries.
        let Some(exe) = resolve_exe_name(process.pid()) else {
            continue;
        };

        // Try matching by executable name first.
        if let Some(app_id) = super::desktop::resolve_app_id(&exe) {
            app_ids.push(app_id);
            continue;
        }

        // Fall back to matching the process cmdline against the full Exec
        // path from .desktop files.  This handles apps whose actual binary
        // name differs from the Exec key (e.g., sandboxed apps like Zed
        // where the running binary is "zed-editor" but Exec is "zed").
        let Ok(cmdline) =
            fs::read(format!("/proc/{}/cmdline", process.pid().as_u32()))
        else {
            continue;
        };

        if let Some(app_id) =
            super::desktop::resolve_app_id_from_cmdline(&cmdline)
        {
            app_ids.push(app_id);
        }
    }

    app_ids.sort();
    app_ids.dedup();
    app_ids
}
