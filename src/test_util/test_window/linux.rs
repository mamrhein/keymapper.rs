// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux test-window backend for the e2e harness.
//!
//! Requires a virtual display (the CI job starts `Xvfb`).  Creates an X11
//! window, advertises its owning PID via `_NET_WM_PID`, and claims it as the
//! active window on the root (`_NET_ACTIVE_WINDOW`) — no window manager is
//! needed to do either.  `get_active_app_name()` then resolves the window's
//! PID to the `.desktop` application id of a fixture that maps this helper's
//! executable name (e.g. `keymapper.testwindow`).

use std::{process, sync::atomic::Ordering, time::Duration};

use x11rb::{
    connection::Connection,
    protocol::xproto::{
        AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass,
    },
    wrapper::ConnectionExt as _,
};

use crate::test_util::monitor::register_signal_handlers;

/// Entry point for the Linux test-window helper.
pub fn run() {
    let (conn, screen) = match x11rb::connect(None) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("testwindow: failed to connect to the X server: {e}");
            process::exit(1);
        }
    };

    let root = conn.setup().roots[screen].root;
    let depth = conn.setup().roots[screen].root_depth;
    let visual = conn.setup().roots[screen].root_visual;

    // Create a visible top-level window.
    let wid = match conn.generate_id() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("testwindow: failed to allocate a window id: {e}");
            process::exit(1);
        }
    };
    if let Err(e) = conn.create_window(
        depth,
        wid,
        root,
        0,
        0,
        320,
        200,
        0,
        WindowClass::INPUT_OUTPUT,
        visual,
        &CreateWindowAux::default(),
    ) {
        eprintln!("testwindow: failed to create the test window: {e}");
        process::exit(1);
    }

    // Advertise our PID so the active-app query can resolve us to a .desktop
    // application id.
    if let Some(atom) = intern_atom(&conn, b"_NET_WM_PID") {
        let _ = conn.change_property32(
            PropMode::REPLACE,
            wid,
            atom,
            AtomEnum::CARDINAL,
            &[process::id()],
        );
    }

    // Map the window so it is visible.
    let _ = conn.map_window(wid);

    // Claim the window as active on the root; there is no window manager in
    // CI to do this for us.
    if let Some(atom) = intern_atom(&conn, b"_NET_ACTIVE_WINDOW") {
        let _ = conn.change_property32(
            PropMode::REPLACE,
            root,
            atom,
            AtomEnum::WINDOW,
            &[wid],
        );
    }

    let _ = conn.flush();

    eprintln!("testwindow: focused the test window");

    let shutdown = register_signal_handlers();

    // Keep the connection alive and process events until a shutdown signal.
    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        let _ = conn.poll_for_event();
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Intern an atom name, returning its XID (u32) or `None` on failure.
fn intern_atom(
    conn: &impl x11rb::connection::RequestConnection,
    name: &[u8],
) -> Option<u32> {
    let cookie = conn.intern_atom(false, name).ok()?;
    let reply = cookie.reply().ok()?;
    (reply.atom != 0).then_some(reply.atom)
}
