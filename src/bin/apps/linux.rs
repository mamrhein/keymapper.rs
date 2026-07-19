// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Lists visible application names on Linux.
//!
//! On X11 the `app_name` is derived from the `WM_CLASS` property (the class
//! name, i.e., the second null-separated string).  This matches what
//! `active-win-pos-rs` returns.
//!
//! On Wayland there is no universal API to enumerate all clients, so a
//! helpful message is printed instead.

use std::env;

use xcb::XidNew;

/// Return true if the session appears to be Wayland rather than X11.
fn is_wayland() -> bool {
    env::var("WAYLAND_DISPLAY").is_ok() && env::var("DISPLAY").is_err()
}

/// Enumerate visible windows via X11 and extract unique `WM_CLASS` values.
fn list_via_x11() -> Vec<String> {
    let Ok((conn, _screen_num)) = xcb::Connection::connect(None) else {
        return Vec::new();
    };

    let setup = conn.get_setup();
    let Some(root_screen) = setup.roots().next() else {
        return Vec::new();
    };
    let root_window = root_screen.root();

    // Look up the `_NET_CLIENT_LIST` atom.
    let client_list_cookie = conn.send_request(&xcb::x::InternAtom {
        only_if_exists: false,
        name: b"_NET_CLIENT_LIST",
    });
    let client_list_atom = match conn.wait_for_reply(client_list_cookie) {
        Ok(reply) => reply.atom(),
        Err(_) => return Vec::new(),
    };

    // Read the list of managed windows.
    let prop_cookie = conn.send_request(&xcb::x::GetProperty {
        delete: false,
        window: root_window,
        property: client_list_atom,
        r#type: xcb::x::ATOM_WINDOW,
        long_offset: 0,
        long_length: 4096,
    });
    let prop_reply = match conn.wait_for_reply(prop_cookie) {
        Ok(reply) => reply,
        Err(_) => return Vec::new(),
    };

    let windows: Vec<u32> = prop_reply.value::<u32>().to_vec();

    // Look up the `WM_CLASS` atom.
    let wm_class_cookie = conn.send_request(&xcb::x::InternAtom {
        only_if_exists: false,
        name: b"WM_CLASS",
    });
    let wm_class_atom = match conn.wait_for_reply(wm_class_cookie) {
        Ok(reply) => reply.atom(),
        Err(_) => return Vec::new(),
    };

    let mut classes: Vec<String> = Vec::new();

    for &window in &windows {
        let prop_cookie = conn.send_request(&xcb::x::GetProperty {
            delete: false,
            window: xcb::x::Window::new(window),
            property: wm_class_atom,
            r#type: xcb::x::ATOM_STRING,
            long_offset: 0,
            long_length: 256,
        });

        let Ok(prop_reply) = conn.wait_for_reply(prop_cookie) else {
            continue;
        };

        let raw = prop_reply.value();
        if raw.is_empty() {
            continue;
        }

        // WM_CLASS contains "instance\0class".  We want the class name
        // (the part after the null byte).  If there is no null byte we
        // fall back to using the whole string.
        let class_name =
            if let Some(null_pos) = raw.iter().position(|&b| b == 0) {
                let after_null = &raw[null_pos + 1..];
                strip_trailing_nuls(after_null)
            } else {
                // No null separator — use the first (and only) string,
                // stripping any trailing nuls.
                strip_trailing_nuls(raw)
            };

        if !class_name.is_empty() {
            classes.push(class_name);
        }
    }

    classes.sort();
    classes.dedup();
    classes
}

/// Strip trailing null bytes from a byte slice and convert to a String.
fn strip_trailing_nuls(bytes: &[u8]) -> String {
    let trimmed = bytes.strip_suffix(&[0]);
    let trimmed = match trimmed {
        Some(t) => t,
        None => bytes,
    };
    String::from_utf8_lossy(trimmed).into_owned()
}

/// Enumerate all visible application names.
pub fn list_app_names() -> Vec<String> {
    if is_wayland() {
        // On Wayland there is no universal cross-compositor API to enumerate
        // all clients.  Print a helpful hint instead of returning an empty
        // list silently.
        eprintln!(
            "Running on Wayland — no universal window enumeration API \
             available."
        );
        eprintln!("Use your compositor's tools to find app IDs:");
        eprintln!(
            "  Hyprland : hyprctl clients -j | jq -r \"[.[].class] | unique \
             | .[]\""
        );
        eprintln!(
            "  Sway     : swaymsg -t get_tree | jq -r \".. | \
             select(.name?)?.app_id // empty\" | sort -u"
        );
        eprintln!("  KDE      : kdotool windows");
        eprintln!();
        return Vec::new();
    }

    list_via_x11()
}
