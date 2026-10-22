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
//! When `$XDG_CURRENT_DESKTOP` names a supported compositor, only that
//! compositor is queried.  Otherwise all are probed in order: KDE (KWin),
//! GNOME (behind the `gnome-eval` feature), COSMIC, then wlroots-based
//! compositors, Hyprland, and finally the XFCE (labwc) foreign-toplevel
//! query.  Each probe is a synchronous, blocking operation that connects,
//! queries, and disconnects immediately.
//!
//! Wherever a compositor reports the active window's owning process (KDE,
//! GNOME), the PID is resolved to the `.desktop` application id via
//! [`super::apps::resolve_process_app_id`] — the same namespace
//! [`super::apps::list_app_names`] produces.  The remaining backends return
//! the app id / class the compositor itself reports for the active window.
//! COSMIC has no D-Bus interface for this; it reports the active window
//! through the `cosmic-toplevel-info` Wayland protocol extension instead
//! (bound at protocol version 1 — see [`cosmic`]).
//!
//! Each compositor backend lives in its own submodule:
//! - [`kde`] — KWin D-Bus query
//! - [`gnome`] — GNOME Shell.Eval introspection
//! - [`cosmic`] — cosmic-toplevel-info Wayland protocol
//! - [`wlroots`] — sway/wlroots IPC socket query
//! - [`hyprland`] — Hyprland control socket query
//! - [`xfce`] — labwc foreign-toplevel Wayland protocol query
//!
//! Shared Wayland connection and JSON parsing helpers live in this parent
//! module.

use std::{
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};

use wayland_client::Connection;

mod cosmic;
mod gnome;
mod hyprland;
mod kde;
mod wlroots;
mod xfce;

use cosmic::query_cosmic;
#[cfg(feature = "gnome-eval")]
use gnome::query_gnome;
use hyprland::query_hyprland;
use kde::query_kde;
use wlroots::query_wlroots;
use xfce::query_xfce;

/// Synchronously query the current foreground application name on Wayland.
///
/// When `$XDG_CURRENT_DESKTOP` names a compositor we have a backend for,
/// only that backend is queried.  Otherwise each compositor backend is
/// tried in order and the first successful result is returned.  Returns
/// `"unknown"` if no backend succeeds.
pub fn get_active_app_name() -> String {
    // Every session type we support exports `$XDG_CURRENT_DESKTOP`, so
    // trust it and skip probing compositors that cannot be the active one.
    if let Some(query) = compositor_from_env() {
        let result = query();
        if !result.is_empty() && result != "unknown" {
            return result;
        }
        return "unknown".to_string();
    }

    // Fallback: probe each compositor in priority order.  The GNOME backend
    // is gated behind the `gnome-eval` feature because it runs JavaScript in
    // the shell context via D-Bus `org.gnome.Shell.Eval`.
    #[cfg(feature = "gnome-eval")]
    let candidates: &[fn() -> String] = &[
        query_kde,
        query_gnome,
        query_cosmic,
        query_wlroots,
        query_hyprland,
        query_xfce,
    ];
    #[cfg(not(feature = "gnome-eval"))]
    let candidates: &[fn() -> String] = &[
        query_kde,
        query_cosmic,
        query_wlroots,
        query_hyprland,
        query_xfce,
    ];

    for query in candidates {
        let result = query();
        if !result.is_empty() && result != "unknown" {
            return result;
        }
    }

    "unknown".to_string()
}

/// Return the compositor backend that `$XDG_CURRENT_DESKTOP` names, if any.
///
/// Returns `None` when the variable is unset or empty, which makes
/// [`get_active_app_name`] fall back to probing every backend.
fn compositor_from_env() -> Option<fn() -> String> {
    compositor_for_desktop(&std::env::var("XDG_CURRENT_DESKTOP").ok()?)
}

/// Map a `$XDG_CURRENT_DESKTOP` value to its compositor backend.
///
/// Per the desktop-entry specification the variable may hold a
/// colon-separated list of names (e.g. `GNOME:ubuntu`); the first known
/// name wins and names are matched case-insensitively.  Returns `None`
/// when no name matches a backend (an unknown desktop, or GNOME without
/// the `gnome-eval` feature), which lets the caller fall back to probing
/// every backend.
///
/// Split out from [`compositor_from_env`] so it can be tested without
/// mutating the process environment.
fn compositor_for_desktop(desktop: &str) -> Option<fn() -> String> {
    desktop.to_ascii_lowercase().split(':').find_map(|name| {
        let backend: fn() -> String = match name.trim() {
            "kde" => query_kde,
            #[cfg(feature = "gnome-eval")]
            "gnome" => query_gnome,
            "cosmic" => query_cosmic,
            // Sway and niri speak the sway-compatible IPC protocol; the
            // backend additionally requires `SWAYSOCK` to be set.
            "sway" | "niri" => query_wlroots,
            "hyprland" => query_hyprland,
            // The XFCE Wayland session runs labwc; the backend queries
            // the foreign-toplevel global labwc advertises.
            "xfce" => query_xfce,
            _ => return None,
        };
        Some(backend)
    })
}

// ---------------------------------------------------------------------------
// Wayland connection (with session-environment fallback)
// ---------------------------------------------------------------------------

/// Connect to the Wayland compositor, falling back to discovering the socket
/// under `$XDG_RUNTIME_DIR` when the usual environment is unavailable.
///
/// The standard path (`WAYLAND_DISPLAY`, or an inherited `WAYLAND_SOCKET`) is
/// tried first.  A daemon started by systemd before the compositor exported
/// `WAYLAND_DISPLAY` has neither, so we then scan `$XDG_RUNTIME_DIR` for
/// `wayland-<N>` sockets and use the lowest display number (the compositor's
/// default display is `wayland-0`).  This is defense-in-depth for a
/// compositor that does not propagate its environment into a systemd user
/// service; the unit normally orders the daemon behind
/// `graphical-session.target` so `WAYLAND_DISPLAY` is already set.
fn connect_wayland() -> Option<Connection> {
    if let Ok(conn) = Connection::connect_to_env() {
        return Some(conn);
    }
    let stream = discover_wayland_stream()?;
    Connection::from_socket(stream).ok()
}

/// Open a [`UnixStream`] to the first reachable compositor socket found under
/// `$XDG_RUNTIME_DIR`.  Returns `None` when the runtime directory is missing
/// or no `wayland-<N>` socket can be reached.
fn discover_wayland_stream() -> Option<UnixStream> {
    let runtime_dir =
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    if !runtime_dir.is_absolute() {
        return None;
    }
    let socket = find_wayland_socket(&runtime_dir)?;
    UnixStream::connect(socket).ok()
}

/// Find the `wayland-<N>` socket with the smallest display number in `dir`.
///
/// Only entries whose name is exactly `wayland-` followed by a number qualify,
/// so related files such as `wayland-0.lock` are ignored.
fn find_wayland_socket(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(u32, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let os_name = entry.file_name();
        let name = os_name.to_string_lossy();
        let Some(suffix) = name.strip_prefix("wayland-") else {
            continue;
        };
        let Ok(display) = suffix.parse::<u32>() else {
            continue;
        };
        // `as_ref` keeps ownership of `best`; only the borrowed display number
        // is inspected before we (possibly) overwrite it.
        if best.as_ref().is_none_or(|(lowest, _)| display < *lowest) {
            best = Some((display, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

// ---------------------------------------------------------------------------
// Shared JSON helpers
// ---------------------------------------------------------------------------

/// Extract a string value for `key` from a JSON response by parsing it with
/// `serde_json` and searching the resulting tree depth-first.
///
/// Replaces the former hand-rolled string scan, which mangled non-ASCII bytes
/// (pushed as `char`, truncating multi-byte UTF-8) and mishandled escapes
/// (`\n` → literal `n`, no `\uXXXX` support, `\"` off-by-one on the
/// backslash).  The depth-first search finds the first string-valued
/// occurrence of `key` at any nesting level, matching the intent of the
/// original first-match scan while correctly skipping non-string values
/// (e.g. `null` `app_id` on the sway tree root).
fn extract_json_field(json: &str, key: &str) -> String {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    find_json_string(&value, key).unwrap_or_default()
}

/// Depth-first search for the first string value associated with `key` in a
/// parsed JSON tree.
fn find_json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if k == key
                    && let Some(s) = v.as_str()
                {
                    return Some(s.to_string());
                }
                if let Some(found) = find_json_string(v, key) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                if let Some(found) = find_json_string(v, key) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, ptr};

    #[cfg(feature = "gnome-eval")]
    use super::query_gnome;
    use super::{
        compositor_for_desktop, extract_json_field, find_wayland_socket,
        query_cosmic, query_hyprland, query_kde, query_wlroots, query_xfce,
    };

    #[test]
    fn finds_lowest_display_number() {
        // Numeric ordering, not lexicographic: `wayland-1` beats `wayland-2`
        // and `wayland-10`.
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "wayland-2",
            "wayland-10",
            "wayland-1",
            "bus",
            "wayland-0.lock",
        ] {
            fs::write(dir.path().join(name), b"").unwrap();
        }
        let found = find_wayland_socket(dir.path()).unwrap();
        assert_eq!(found.file_name().unwrap(), "wayland-1");
    }

    #[test]
    fn ignores_non_socket_entries() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["bus", "pipewire-0", "wayland-0.lock", "wayland-"] {
            fs::write(dir.path().join(name), b"").unwrap();
        }
        assert!(find_wayland_socket(dir.path()).is_none());
    }

    #[test]
    fn missing_dir_returns_none() {
        let dir = std::path::Path::new("/nonexistent-keymapper-runtime-dir");
        assert!(find_wayland_socket(dir).is_none());
    }

    // -----------------------------------------------------------------------
    // compositor_for_desktop (XDG_CURRENT_DESKTOP → backend mapping)
    // -----------------------------------------------------------------------

    /// Compare backends by address without calling them; a live query would
    /// hit D-Bus or a compositor socket.
    fn selects(desktop: &str, expected: fn() -> String) -> bool {
        compositor_for_desktop(desktop)
            .is_some_and(|query| ptr::fn_addr_eq(query, expected))
    }

    #[test]
    fn desktop_names_select_matching_backend() {
        // Names are matched case-insensitively; these are the values the
        // respective sessions export.
        assert!(selects("KDE", query_kde));
        assert!(selects("COSMIC", query_cosmic));
        assert!(selects("sway", query_wlroots));
        assert!(selects("niri", query_wlroots));
        assert!(selects("Hyprland", query_hyprland));
        assert!(selects("XFCE", query_xfce));
        assert!(selects("XFCE:labwc", query_xfce));
    }

    #[cfg(feature = "gnome-eval")]
    #[test]
    fn desktop_names_select_gnome_with_feature() {
        assert!(selects("GNOME", query_gnome));
    }

    #[cfg(not(feature = "gnome-eval"))]
    #[test]
    fn gnome_without_feature_probes_all() {
        // Without the gated backend there is nothing to select, so the
        // caller falls back to probing every compositor.
        assert!(compositor_for_desktop("GNOME").is_none());
    }

    #[test]
    fn colon_separated_list_finds_known_name() {
        // Fedora/Ubuntu-style values carry a flavor token next to the
        // desktop name; the first known name in the list wins.
        assert!(selects("KDE:Plasma", query_kde));
        assert!(selects("ubuntu:sway", query_wlroots));
    }

    #[test]
    fn unknown_or_empty_desktop_probes_all() {
        assert!(compositor_for_desktop("").is_none());
        assert!(compositor_for_desktop("wayfire").is_none());
    }

    // -----------------------------------------------------------------------
    // extract_json_field (R3-27: serde_json replaces hand-rolled scan)
    // -----------------------------------------------------------------------

    #[test]
    fn extract_json_string_simple() {
        // Flat JSON: a string value for the requested key.
        let json = r#"{"class": "org.gnome.Terminal", "title": "Terminal"}"#;
        assert_eq!(extract_json_field(json, "class"), "org.gnome.Terminal");
    }

    #[test]
    fn extract_json_string_nested_depth_first() {
        // The sway `get_tree` reply is deeply nested; the first string-
        // valued `app_id` at any depth wins, matching the original first-
        // match scan intent.
        let json = r#"{"name": "root", "app_id": null, "nodes": [
            {"name": "a", "app_id": null, "nodes": []},
            {"name": "b", "app_id": "terminal", "nodes": []}
        ]}"#;
        assert_eq!(extract_json_field(json, "app_id"), "terminal");
    }

    #[test]
    fn extract_json_string_handles_non_ascii_and_escapes() {
        // The old hand-rolled scan pushed raw bytes as `char`, truncating
        // multi-byte UTF-8 and turning `\n` into a literal `n`.  serde_json
        // decodes escapes and preserves the original Unicode scalar value.
        let json = r#"{"app_id": "café\u00e9 \"quoted\" tab\t"}"#;
        assert_eq!(
            extract_json_field(json, "app_id"),
            "caféé \"quoted\" tab\t"
        );
    }

    #[test]
    fn extract_json_string_skips_non_string_values() {
        // A `null` or numeric value for the key is skipped, not returned as
        // an empty string, so the search continues deeper.
        let json =
            r#"{"app_id": null, "nodes": [{"app_id": 42, "name": "x"}]}"#;
        assert_eq!(extract_json_field(json, "app_id"), "");
    }

    #[test]
    fn extract_json_string_missing_key_returns_empty() {
        let json = r#"{"title": "no app_id here"}"#;
        assert_eq!(extract_json_field(json, "app_id"), "");
    }

    #[test]
    fn extract_json_string_invalid_json_returns_empty() {
        // Truncated or malformed JSON must not panic.
        assert_eq!(extract_json_field("not json at all", "app_id"), "");
        assert_eq!(extract_json_field("{\"app_id\":", "app_id"), "");
    }
}
