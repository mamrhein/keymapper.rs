// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Hyprland — control socket query.
//!
//! Hyprland exposes its IPC via a socket at:
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`
//! Using `XDG_RUNTIME_DIR` (a per-user, mode-0700 directory) instead of
//! `/tmp` prevents an attacker from pre-creating a spoofed socket path
//! in a world-writable location.

use std::{
    io::{Read, Write},
    time::Duration,
};

use super::extract_json_field;

/// Query the active window on Hyprland via its control socket.
///
/// Sends the `j/activewindow` command and extracts the `class` field
/// (the X11 `WM_CLASS` equivalent) from the JSON response.
pub(super) fn query_hyprland() -> String {
    let signature = match std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        Ok(s) => s,
        Err(_) => return String::new(),
    };

    let runtime_dir = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(d) if !d.is_empty() && std::path::Path::new(&d).is_absolute() => d,
        _ => return String::new(),
    };

    let socket = format!("{runtime_dir}/hypr/{signature}/.socket2.sock");
    if !std::path::Path::new(&socket).exists() {
        return String::new();
    }

    // Send the "j/activewindow" command to get JSON output about the
    // active window.
    let mut stream = match std::os::unix::net::UnixStream::connect(&socket) {
        Ok(s) => {
            s.set_read_timeout(Some(Duration::from_millis(50))).ok();
            s
        }
        Err(_) => return String::new(),
    };

    if stream.write_all(b"j/activewindow\n").is_err() {
        return String::new();
    }

    let mut response = Vec::new();
    if stream.read_to_end(&mut response).is_err() {
        return String::new();
    }

    // Parse the JSON response for the "class" field (X11 WM_CLASS
    // equivalent).
    let Ok(json) = std::str::from_utf8(&response) else {
        return String::new();
    };

    extract_json_field(json, "class")
}
