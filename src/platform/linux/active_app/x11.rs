// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! X11 active window query via EWMH `_NET_ACTIVE_WINDOW`.
//!
//! Connects to the X server, resolves the active window from the root
//! window's `_NET_ACTIVE_WINDOW` property, then reads `_NET_WM_NAME`
//! (falling back to `WM_NAME`) from that window.

use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, Connection as _, GetPropertyCookie, Window},
    xapi::Atomic,
};

/// Synchronously query the current foreground application name on X11.
///
/// Returns `"unknown"` if the X connection fails or no active window is set.
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
    let active_window =
        match get_window_property_atom(&conn, root_window, net_active_atom) {
            Ok(Some(wid)) if wid != 0 => wid,
            _ => return "unknown".to_string(),
        };

    // Step 3: Read _NET_WM_NAME (UTF-8) from the active window.
    let utf8_name_atom = match intern_atom(&conn, b"_NET_WM_NAME") {
        Ok(atom) if atom != 0 => atom,
        _ => return "unknown".to_string(),
    };

    if let Some(name) =
        get_window_property_string(&conn, active_window, utf8_name_atom)
    {
        return name;
    }

    // Step 4: Fall back to WM_NAME (core window name, Latin-1).
    if let Some(name) = get_window_property_string(
        &conn,
        active_window,
        AtomEnum::WM_NAME.into(),
    ) {
        return name;
    }

    "unknown".to_string()
}

/// Intern an atom name, returning its XID (u32).
fn intern_atom<C: Connection>(
    conn: &C,
    name: &[u8],
) -> Result<u32, x11rb::protocol::xerror::X11Error> {
    use x11rb::protocol::xproto::Connection as _;

    let cookie = conn.intern_atom(false, name)?;
    let reply = conn.wait_for_reply(cookie)?;
    Ok(reply.atom)
}

/// Query a window property that holds an ATOM value.
fn get_window_property_atom<C: Connection>(
    conn: &C,
    window: Window,
    property_atom: u32,
) -> Result<Option<u32>, x11rb::protocol::xerror::X11Error>
where
    GetPropertyCookie: Atomic<C>,
{
    use x11rb::protocol::xproto::Connection as _;

    let cookie = conn.get_property(
        false,
        window,
        property_atom,
        AtomEnum::ATOM,
        0,
        1, // request 1 ATOM (4 bytes).
    )?;
    let reply = conn.wait_for_reply(cookie)?;

    if reply.value_len > 0 {
        // The value is a sequence of 4-byte integers (ATOMs).
        Ok(reply.value32().first().copied())
    } else {
        Ok(None)
    }
}

/// Query a window property that holds raw bytes, interpreting them as UTF-8.
fn get_window_property_string<C: Connection>(
    conn: &C,
    window: Window,
    property_atom: u32,
) -> Option<String>
where
    GetPropertyCookie: Atomic<C>,
{
    use x11rb::protocol::xproto::Connection as _;

    // Request up to 4096 32-bit chunks (16 KiB), more than enough for a
    // window title.
    let cookie = conn
        .get_property(
            false,
            window,
            property_atom,
            AtomEnum::ANY_PROP_TYPE,
            0,
            4096,
        )
        .ok()?;

    let reply = conn.wait_for_reply(cookie).ok()?;
    if reply.value_len == 0 {
        return None;
    }

    // The property value is raw bytes.  _NET_WM_NAME is UTF-8; WM_NAME is
    // Latin-1, but we decode both as lossy UTF-8 for simplicity.
    Some(String::from_utf8_lossy(&reply.value).into_owned())
}
