// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux application identity.
//!
//! The foreground application is detected via `$XDG_SESSION_TYPE` and
//! delegated to the appropriate backend (X11 or Wayland).  The list of
//! visible applications is produced by scanning `/proc` for GUI processes
//! (see the `apps` module) and resolving them against `.desktop` files.

mod apps;
mod desktop;
mod wayland;
mod x11;

pub(crate) use apps::list_app_names;

/// Synchronously query the current foreground application name.
///
/// Detects the display server and delegates to the appropriate backend.
/// Returns `"unknown"` if no suitable backend is available or the query fails.
pub fn get_active_app_name() -> String {
    // Check the session type to determine which backend to use.
    let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();

    match session_type.as_str() {
        "x11" => x11::get_active_app_name(),
        "wayland" => wayland::get_active_app_name(),
        _ => {
            // Unknown or unset session type; try backends in order.
            let result = x11::get_active_app_name();
            if result != "unknown" {
                return result;
            }
            wayland::get_active_app_name()
        }
    }
}
