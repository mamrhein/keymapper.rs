// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux foreground application query.
//!
//! Detects the active display server via `$XDG_SESSION_TYPE` and delegates
//! to the appropriate backend (X11 or Wayland).

mod wayland;
mod x11;

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
