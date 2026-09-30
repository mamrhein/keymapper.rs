// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! X11 active application query via EWMH `_NET_ACTIVE_WINDOW`.
//!
//! Connects to the X server, resolves the active window from the root
//! window's `_NET_ACTIVE_WINDOW` property, then reads `_NET_WM_PID` from
//! that window and resolves the owning process to its `.desktop`
//! application id — the same namespace [`super::apps::list_app_names`]
//! produces.
//!
//! The window title is deliberately _not_ used: it changes as the document
//! changes and would never match an `appnames` value.

use x11rb::{
    connection::{Connection, RequestConnection},
    errors::ReplyError,
    protocol::xproto::{ConnectionExt as _, GetPropertyType, Window},
};

/// Synchronously query the current foreground application name on X11.
///
/// Resolves the active window's owning process (`_NET_WM_PID`) to its
/// `.desktop` application id.  Returns `"unknown"` if the X connection
/// fails, no active window is set, the window has no `_NET_WM_PID`, or the
/// process cannot be resolved against a `.desktop` file.
pub fn get_active_app_name() -> String {
    let (conn, screen_num) = match x11rb::connect(None) {
        Ok(result) => result,
        Err(_) => return "unknown".to_string(),
    };

    let root_window = conn.setup().roots[screen_num].root;

    // Step 1: Intern the _NET_ACTIVE_WINDOW atom.
    let net_active_atom = match intern_atom(&conn, b"_NET_ACTIVE_WINDOW") {
        Ok(atom) if atom != 0 => atom,
        _ => return "unknown".to_string(),
    };

    // Step 2: Query the root window for the active window ID.
    let Some(active_window) =
        get_window_property_u32(&conn, root_window, net_active_atom)
            .filter(|&wid| wid != 0)
    else {
        return "unknown".to_string();
    };

    // Step 3: Read the owning process from _NET_WM_PID.
    let wm_pid_atom = match intern_atom(&conn, b"_NET_WM_PID") {
        Ok(atom) if atom != 0 => atom,
        _ => return "unknown".to_string(),
    };

    let Some(pid) = get_window_property_u32(&conn, active_window, wm_pid_atom)
        .filter(|&pid| pid != 0)
    else {
        return "unknown".to_string();
    };

    // Step 4: Resolve the process to the same .desktop application id
    // namespace as list_app_names.
    super::apps::resolve_process_app_id(pid)
        .unwrap_or_else(|| "unknown".to_string())
}

/// Intern an atom name, returning its XID (u32).
fn intern_atom<C: RequestConnection>(
    conn: &C,
    name: &[u8],
) -> Result<u32, ReplyError> {
    let cookie = conn.intern_atom(false, name)?;
    let reply = cookie.reply()?;
    Ok(reply.atom)
}

/// Query a window property that holds a single 32-bit value (a window id or
/// a pid).
///
/// `GetPropertyType::ANY` is used because EWMH declares `_NET_ACTIVE_WINDOW`
/// as `WINDOW` and `_NET_WM_PID` as `CARDINAL`, but both values are 4 bytes
/// wide and only the value is of interest here.
fn get_window_property_u32<C: RequestConnection>(
    conn: &C,
    window: Window,
    property_atom: u32,
) -> Option<u32> {
    let cookie = conn
        .get_property(
            false,
            window,
            property_atom,
            GetPropertyType::ANY,
            0,
            1, // request 1 32-bit chunk (4 bytes).
        )
        .ok()?;

    let reply = cookie.reply().ok()?;
    if reply.value_len > 0 {
        // The value is a single 4-byte integer.
        reply.value32().and_then(|mut iter| iter.next())
    } else {
        None
    }
}
