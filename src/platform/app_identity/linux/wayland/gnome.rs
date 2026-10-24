// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! GNOME — active window via a Shell extension bridge.
//!
//! The `org.gnome.Shell.Eval` D-Bus interface was removed from newer GNOME
//! releases, so introspecting the shell directly is no longer possible.
//! Instead a small shell extension exports the focused window's owning
//! application over a dedicated D-Bus method.
//!
//! This module is a pure client of that bridge: the extension itself
//! (sources in `resources/gnome-ext/`) is installed at setup time by
//! `scripts/install-gnome-ext.sh`, which `scripts/install-linux.sh` runs on
//! GNOME.  To remove it again, run `gnome-extensions uninstall
//! local.keymapper.active-window`.
//!
//! # Activation timing under GNOME ≥ 45
//!
//! GNOME ≥ 45 scans the extensions directories only at session start, so a
//! freshly installed extension goes live at the next login.  Until then the
//! backend answers `""` and the normal fallback chain applies.  The daemon
//! may also start before the Shell has finished loading an already-enabled
//! extension, so the very first query waits at most once (up to
//! [`LOAD_TIMEOUT`]) for the extension's D-Bus interface to appear; later
//! queries are single round-trips.

use std::{
    sync::atomic::{AtomicBool, Ordering::Relaxed},
    thread,
    time::{Duration, Instant},
};

/// D-Bus endpoint exported by the extension inside the Shell process.
const DBUS_SERVICE: &str = "org.gnome.Shell";
const DBUS_PATH: &str = "/org/gnome/shell/extensions/ActiveWindow";
const DBUS_INTERFACE: &str = "org.gnome.shell.extensions.ActiveWindow";

/// How long the first query waits for the Shell to load the extension and
/// export its D-Bus interface.  Later queries never wait.
const LOAD_TIMEOUT: Duration = Duration::from_millis(1500);
/// Poll interval while waiting for the extension to become available.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Set once a `GetActiveWindow` call has succeeded in this session, so
/// steady-state queries never take the warm-up path.
static LOADED: AtomicBool = AtomicBool::new(false);

/// Set once the one-time load-wait budget has been spent, so a missing
/// extension (not installed, Shell not running) costs a single round-trip
/// per query instead of [`LOAD_TIMEOUT`].
static WARMUP_DONE: AtomicBool = AtomicBool::new(false);

/// Query the active window on GNOME through the Shell extension.
///
/// Each query is a plain `GetActiveWindow` D-Bus call; only the very first
/// one may wait (up to [`LOAD_TIMEOUT`], once) for the Shell to finish
/// loading the extension.  Returns the `.desktop` application id (without
/// the `.desktop` suffix, matching
/// [`super::super::apps::resolve_process_app_id`]), or an empty string when
/// GNOME Shell is not running, the extension is not installed or not loaded
/// (yet), or no window has focus.
pub(super) fn query_gnome() -> String {
    use zbus::blocking::Connection;

    let Ok(conn) = Connection::session() else {
        return String::new();
    };

    match call_get_active_window(&conn) {
        Ok(reply) => parse_reply(reply),
        Err(_) if LOADED.load(Relaxed) || WARMUP_DONE.swap(true, Relaxed) => {
            // Steady state: the extension is (or stays) unavailable; do not
            // burn the load timeout again.
            String::new()
        }
        Err(_) => {
            // One-time warm-up: the extension may have just been enabled at
            // session start, and loading is asynchronous.  Outside a GNOME
            // session there is nothing to wait for.
            if !shell_is_running(&conn).unwrap_or(false) {
                return String::new();
            }
            let deadline = Instant::now() + LOAD_TIMEOUT;
            loop {
                match call_get_active_window(&conn) {
                    Ok(reply) => return parse_reply(reply),
                    Err(_) if Instant::now() < deadline => {
                        thread::sleep(POLL_INTERVAL)
                    }
                    Err(_) => return String::new(),
                }
            }
        }
    }
}

/// Deserialize a successful reply and record the extension as active.
fn parse_reply(reply: zbus::Message) -> String {
    LOADED.store(true, Relaxed);
    let Ok(json) = reply.body().deserialize::<String>() else {
        return String::new();
    };
    parse_app_id(&json)
}

/// Issue a single `GetActiveWindow` call on `conn`.
fn call_get_active_window(
    conn: &zbus::blocking::Connection,
) -> Result<zbus::Message, zbus::Error> {
    conn.call_method(
        Some(DBUS_SERVICE),
        DBUS_PATH,
        Some(DBUS_INTERFACE),
        "GetActiveWindow",
        &(),
    )
}

/// Ask the session bus whether GNOME Shell currently owns
/// `org.gnome.Shell`.
fn shell_is_running(
    conn: &zbus::blocking::Connection,
) -> Result<bool, zbus::Error> {
    let reply = conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "NameHasOwner",
        &(DBUS_SERVICE,),
    )?;
    reply.body().deserialize()
}

/// Extract the application id from the extension's JSON reply.
///
/// The extension reports `{"app_id": "firefox.desktop", ...}` for the
/// focused window, `{"error": ...}` when no window has focus, and the
/// literal `"unknown"` when the window has no known application.  The
/// keymapper namespace uses the `.desktop` file stem, so the suffix is
/// stripped; anything that is not a concrete app id maps to the empty
/// string.
fn parse_app_id(json: &str) -> String {
    let app_id = super::extract_json_field(json, "app_id");
    match app_id.as_str() {
        "" | "unknown" => String::new(),
        _ => app_id
            .strip_suffix(".desktop")
            .unwrap_or(&app_id)
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::{LOAD_TIMEOUT, parse_app_id, query_gnome};

    #[test]
    fn strips_desktop_suffix() {
        let json =
            r#"{"title": "Zed", "wm_class": "Zed", "app_id": "zed.desktop"}"#;
        assert_eq!(parse_app_id(json), "zed");
    }

    #[test]
    fn keeps_reverse_domain_ids_intact() {
        let json = r#"{"app_id": "org.mozilla.firefox.desktop"}"#;
        assert_eq!(parse_app_id(json), "org.mozilla.firefox");
    }

    #[test]
    fn id_without_suffix_is_kept() {
        let json = r#"{"app_id": "some-app"}"#;
        assert_eq!(parse_app_id(json), "some-app");
    }

    #[test]
    fn unknown_and_error_replies_map_to_empty() {
        // No known application behind the focused window.
        assert_eq!(parse_app_id(r#"{"app_id": "unknown"}"#), "");
        // No focused window at all (desktop background).
        assert_eq!(parse_app_id(r#"{"error": "No active window found"}"#), "");
        assert_eq!(parse_app_id("not json at all"), "");
    }

    #[test]
    fn does_not_panic() {
        // Outside a GNOME session the probe returns an empty string, never
        // the `"unknown"` sentinel (that is produced by
        // `get_active_app_name`).
        let result = query_gnome();
        assert_ne!(result, "unknown");
    }

    #[test]
    fn repeat_queries_are_fast() {
        // The one-time load wait is spent at most once per process; further
        // calls must be plain D-Bus round-trips.
        let _ = query_gnome();
        let start = Instant::now();
        for _ in 0..5 {
            let _ = query_gnome();
        }
        // Five queries are five D-Bus round-trips; even on a loaded system
        // they stay well under the load timeout spent on the first call.
        assert!(
            start.elapsed() < LOAD_TIMEOUT,
            "repeat queries took {:?}, expected well under {LOAD_TIMEOUT:?}",
            start.elapsed(),
        );
    }
}
