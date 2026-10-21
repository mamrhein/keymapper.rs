// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! GNOME — D-Bus Shell.Eval introspection.
//!
//! GNOME Shell exposes a read-only Eval interface that can run JavaScript
//! in the shell context.  We use it to query the focused window's owning
//! PID and resolve it against the `.desktop` files.  The `wm_class` alone is
//! not used because it is not reliably a `.desktop` application id.

#[cfg(feature = "gnome-eval")]
use super::super::apps;

/// Query the active window on GNOME via the `org.gnome.Shell.Eval` D-Bus
/// interface.
///
/// This backend is gated behind the `gnome-eval` feature because it executes
/// JavaScript in the GNOME Shell process context.
#[cfg(feature = "gnome-eval")]
pub(super) fn query_gnome() -> String {
    use zbus::blocking::Connection;

    let conn = match Connection::session() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

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
        // Verify that /proc/<pid> is owned by the current uid before
        // trusting the PID returned from the shell — a compromised shell or
        // a spoofed reply could otherwise point at another user's process.
        if !apps::proc_owner_is_self(pid as u32) {
            return String::new();
        }
        return apps::resolve_process_app_id(pid as u32).unwrap_or_default();
    }

    String::new()
}
