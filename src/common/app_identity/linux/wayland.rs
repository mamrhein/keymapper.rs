// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Wayland active window query via compositor-specific mechanisms.
//!
//! Probes compositors in order: KDE (KWin), GNOME, COSMIC, then
//! wlroots-based compositors and Hyprland.  Each probe is a synchronous,
//! blocking operation that connects, queries, and disconnects immediately.
//!
//! Wherever a compositor reports the active window's owning process (KDE,
//! GNOME), the PID is resolved to the `.desktop` application id via
//! [`super::apps::resolve_process_app_id`] — the same namespace
//! [`super::apps::list_app_names`] produces.  The remaining backends return
//! the app id / class the compositor itself reports for the active window.

use std::time::Duration;

/// Synchronously query the current foreground application name on Wayland.
///
/// Tries each compositor backend in order and returns the first successful
/// result.  Returns `"unknown"` if no backend succeeds.
pub fn get_active_app_name() -> String {
    // Probe each compositor in priority order.
    let candidates: [fn() -> String; 5] = [
        query_kde,
        query_gnome,
        query_cosmic,
        query_wlroots,
        query_hyprland,
    ];

    for query in candidates {
        let result = query();
        if !result.is_empty() && result != "unknown" {
            return result;
        }
    }

    "unknown".to_string()
}

// ---------------------------------------------------------------------------
// KDE (KWin) — D-Bus synchronous query
// ---------------------------------------------------------------------------

fn query_kde() -> String {
    use zbus::blocking::Connection;

    let conn = match Connection::session() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    // KWin's Workspace3 interface exposes activeWindow() which returns
    // the PID of the active window.
    let pid: i32 = match conn.call_method(
        Some("org.kde.KWin"),
        "/KWin",
        Some("org.kde.kwin.Workspace3"),
        "activeWindow",
        &(),
    ) {
        Ok(reply) => match reply.body().deserialize() {
            Ok(v) => v,
            Err(_) => return String::new(),
        },
        Err(_) => return String::new(),
    };

    if pid > 0 {
        return super::apps::resolve_process_app_id(pid as u32)
            .unwrap_or_default();
    }

    String::new()
}

// ---------------------------------------------------------------------------
// GNOME — D-Bus Shell.Eval introspection
// ---------------------------------------------------------------------------

fn query_gnome() -> String {
    use zbus::blocking::Connection;

    let conn = match Connection::session() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    // GNOME Shell exposes a read-only Eval interface that can run
    // JavaScript in the shell context.  We use it to query the focused
    // window's owning PID and resolve it against the .desktop files.  The
    // wm_class alone is not used because it is not reliably a .desktop
    // application id.
    let js = "global.display.focus_window && \
              global.display.focus_window.get_pid() || 0";

    let pid: i32 = match conn.call_method(
        Some("org.gnome.Shell"),
        "/org/gnome/Shell",
        Some("org.gnome.Shell.Eval"),
        "Eval",
        &(js,),
    ) {
        Ok(reply) => match reply.body().deserialize() {
            Ok(v) => v,
            Err(_) => return String::new(),
        },
        Err(_) => return String::new(),
    };

    if pid > 0 {
        return super::apps::resolve_process_app_id(pid as u32)
            .unwrap_or_default();
    }

    String::new()
}

// ---------------------------------------------------------------------------
// COSMIC — D-Bus fallback (pop-os specific)
// ---------------------------------------------------------------------------

fn query_cosmic() -> String {
    use zbus::blocking::Connection;

    let conn = match Connection::session() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    // Check if COSMIC is running by attempting to connect to its D-Bus
    // service.  Current COSMIC builds (com.system76.CosmicComp) do not
    // expose an active-window interface yet, so this probe is a
    // best-effort forward-looking query that simply fails until one
    // appears.
    let app_id: String = match conn.call_method(
        Some("com.system76.CosmicDesktop"),
        "/org/freedesktop/Portal/v1",
        Some("org.freedesktop.portal.Foreground"),
        "ActiveWindow",
        &(),
    ) {
        Ok(reply) => match reply.body().deserialize() {
            Ok(v) => v,
            Err(_) => return String::new(),
        },
        Err(_) => return String::new(),
    };

    if !app_id.is_empty() {
        return app_id;
    }

    String::new()
}

// ---------------------------------------------------------------------------
// wlroots-based compositors (sway, niri) — IPC socket query
// ---------------------------------------------------------------------------

fn query_wlroots() -> String {
    // Sway and other wlroots-based compositors expose a JSON IPC protocol
    // over a Unix domain socket.
    let socket = std::env::var("SWAYSOCK").unwrap_or_else(|_| {
        std::env::var("XDG_RUNTIME_DIR")
            .map(|rd| format!("{rd}/sway-ipc.sock"))
            .unwrap_or_else(|_| "/run/user/1000/sway-ipc.sock".to_string())
    });

    if !std::path::Path::new(&socket).exists() {
        return String::new();
    }

    // Build the IPC get_tree message: length-prefixed binary protocol.
    let payload = r#"{"payload":"", "cmd":"get_tree"}"#;

    let mut msg = Vec::with_capacity(12 + payload.len());
    msg.extend_from_slice(b"sway"); // magic
    msg.extend_from_slice(&4u32.to_le_bytes()); // version
    msg.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // length
    msg.extend_from_slice(payload.as_bytes()); // payload

    query_wlroots_socket(&socket, &msg)
}

fn query_wlroots_socket(socket: &str, request: &[u8]) -> String {
    use std::io::{Read, Write};

    let mut stream = match std::os::unix::net::UnixStream::connect(socket) {
        Ok(s) => {
            s.set_read_timeout(Some(Duration::from_millis(50))).ok();
            s
        }
        Err(_) => return String::new(),
    };

    if stream.write_all(request).is_err() {
        return String::new();
    }

    let mut response = Vec::new();
    if stream.read_to_end(&mut response).is_err() {
        return String::new();
    }

    // The IPC response uses the same length-prefixed binary protocol as the
    // request: 4-byte "sway" magic, 4-byte version, 4-byte payload length,
    // then the JSON payload.
    if response.len() < 12 {
        return String::new();
    }

    // Verify the magic bytes.
    if &response[0..4] != b"sway" {
        return String::new();
    }

    let payload_len = u32::from_le_bytes([
        response[8],
        response[9],
        response[10],
        response[11],
    ]) as usize;

    if response.len() < 12 + payload_len {
        return String::new();
    }

    let json = &response[12..12 + payload_len];
    let Ok(json_str) = std::str::from_utf8(json) else {
        return String::new();
    };
    extract_json_string(json_str, "app_id")
}

// ---------------------------------------------------------------------------
// Hyprland — control socket query
// ---------------------------------------------------------------------------

fn query_hyprland() -> String {
    // Hyprland exposes its IPC via a socket at:
    // /tmp/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock
    let signature = match std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        Ok(s) => s,
        Err(_) => return String::new(),
    };

    let socket = format!("/tmp/hypr/{signature}/.socket2.sock");
    if !std::path::Path::new(&socket).exists() {
        return String::new();
    }

    // Send the "j/activewindow" command to get JSON output about the
    // active window.
    use std::io::{Read, Write};

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

    // Parse the JSON response for the "class" field (X11 WM_CLASS equivalent).
    let Ok(json) = std::str::from_utf8(&response) else {
        return String::new();
    };

    extract_json_string(json, "class")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Simple helper to extract a string value for a given key from a JSON-like
/// string.  Not a full parser — sufficient for the flat JSON responses from
/// compositor IPC sockets.
fn extract_json_string(json: &str, key: &str) -> String {
    let pattern = format!("\"{key}\"");
    let start = match json.find(&pattern) {
        Some(pos) => pos,
        None => return String::new(),
    };

    let after_key = &json[start + pattern.len()..];

    // Skip whitespace and the colon.
    let after_colon = match after_key.find(':') {
        Some(pos) => &after_key[pos + 1..],
        None => return String::new(),
    };

    // Find the opening quote of the value.
    let trimmed = after_colon.trim_start();
    let value_start = match trimmed.find('"') {
        Some(pos) => &trimmed[pos + 1..],
        None => return String::new(),
    };

    // Find the closing quote, handling escaped quotes.
    let mut result = String::new();
    let mut bytes = value_start.bytes();

    while let Some(b) = bytes.next() {
        if b == b'\\' {
            // Escaped character — take the next byte literally.
            if let Some(next) = bytes.next() {
                result.push(next as char);
            }
        } else if b == b'"' {
            // End of string.
            return result;
        } else {
            result.push(b as char);
        }
    }

    result
}
