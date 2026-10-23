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
//! Instead a small shell extension (sources in `gnome_ext/`) exports the
//! focused window's owning application over a dedicated D-Bus method.
//!
//! # Lifecycle under GNOME ≥ 45
//!
//! GNOME ≥ 45 scans the extensions directories once per session and offers
//! no D-Bus call to rescan (`Eval` would be the only way, and that is the
//! interface being replaced).  A *newly written* extension directory is
//! therefore only picked up at the next login; `EnableExtension`/
//! `DisableExtension` work dynamically for extensions the session already
//! knows.  Accordingly:
//!
//! - The extension is installed **once**, on the first query; later queries
//!   are a single `GetActiveWindow` D-Bus round-trip.
//! - Enabling goes through the `gnome-extensions` CLI.  While the Shell has
//!   not scanned the directory yet it reports the extension as unknown, so the
//!   uuid is appended to the `enabled-extensions` setting directly; the Shell
//!   then loads it at the next login.  Until then the backend answers
//!   `"unknown"` via the normal fallback chain.
//! - The RAII guard uninstalls on process exit only when the extension was
//!   actually active during this session.  A still-pending install must
//!   survive logout (which stops the daemon): removing it then would undo the
//!   very activation the logout is needed for.  Remove by hand with
//!   `gnome-extensions disable local.keymapper.active-window` followed by
//!   deleting the extension directory.

use std::{
    path::PathBuf,
    process::Command,
    sync::{
        Mutex, Once, PoisonError,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
    thread,
    time::{Duration, Instant},
};

use fs_err as fs;
use thiserror::Error;

/// UUID of the bundled extension; must match `gnome_ext/metadata.json`.
const EXTENSION_UUID: &str = "local.keymapper.active-window";
const METADATA_CONTENT: &str = include_str!("gnome_ext/metadata.json");
const EXTENSION_CONTENT: &str = include_str!("gnome_ext/extension.js");

/// D-Bus endpoint exported by the extension inside the Shell process.
const DBUS_SERVICE: &str = "org.gnome.Shell";
const DBUS_PATH: &str = "/org/gnome/shell/extensions/ActiveWindow";
const DBUS_INTERFACE: &str = "org.gnome.shell.extensions.ActiveWindow";

/// GSettings key holding the list of enabled extension uuids; written
/// directly when the Shell does not know the extension (yet).
const ENABLED_EXTENSIONS_SCHEMA: &str = "org.gnome.shell";
const ENABLED_EXTENSIONS_KEY: &str = "enabled-extensions";

/// How long the first query waits for the Shell to load the extension and
/// export its D-Bus interface.  Later queries never wait.
const LOAD_TIMEOUT: Duration = Duration::from_millis(1500);
/// Poll interval while waiting for the extension to become available.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The installed extension guard, created on the first query and dropped by
/// the `atexit` handler on normal process exit.  The mutex only serializes
/// installation; queries do not hold it.
static GUARD: Mutex<Option<GnomeExtensionGuard>> = Mutex::new(None);

/// Set once the extension is installed in this process (the fast path that
/// keeps steady-state queries mutex-free).
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Set once a `GetActiveWindow` call has succeeded in this session.
static LOADED: AtomicBool = AtomicBool::new(false);

/// Set once the one-time load-wait budget has been spent, so a never-
/// loading extension (non-GNOME session, Shell not running) costs a single
/// round-trip per query instead of [`LOAD_TIMEOUT`].
static WARMUP_DONE: AtomicBool = AtomicBool::new(false);

/// Ensures the `atexit` handler that drops the guard is registered exactly
/// once.
static ATEXIT_REGISTERED: Once = Once::new();

#[derive(Debug, Error)]
enum GnomeError {
    #[error("cannot locate the user data directory")]
    NoDataDir,
    // `fs-err` reports through `std::io::Error`, carrying the path context.
    #[error("I/O error managing the extension files")]
    Io(#[from] std::io::Error),
    #[error("D-Bus error: {0}")]
    Bus(#[from] zbus::Error),
}

/// RAII guard that manages the lifecycle of the GNOME extension.
///
/// Installing it writes the extension into
/// `$XDG_DATA_HOME/gnome-shell/extensions/<uuid>/` and enables it (see the
/// [module docs](self) for the GNOME ≥ 45 activation timing).  Dropping it
/// uninstalls the extension if it was active during this session; a pending
/// install is deliberately kept so that the next login completes the setup.
struct GnomeExtensionGuard {
    ext_dir: PathBuf,
}

impl GnomeExtensionGuard {
    /// Installs the extension, idempotently: files are only written when
    /// they differ from the bundled contents (version upgrades, partial
    /// writes from an interrupted run), and enabling is best effort.
    fn install() -> Result<Self, GnomeError> {
        let data_dir = dirs::data_dir().ok_or(GnomeError::NoDataDir)?;
        let ext_dir = data_dir
            .join("gnome-shell")
            .join("extensions")
            .join(EXTENSION_UUID);

        if !files_current(&ext_dir) {
            fs::create_dir_all(&ext_dir)?;
            fs::write(ext_dir.join("metadata.json"), METADATA_CONTENT)
                .inspect_err(|_| {
                    // Leave no half-written directory behind.
                    let _ = fs::remove_dir_all(&ext_dir);
                })?;
            fs::write(ext_dir.join("extension.js"), EXTENSION_CONTENT)
                .inspect_err(|_| {
                    let _ = fs::remove_dir_all(&ext_dir);
                })?;
        }

        enable_extension();
        Ok(Self { ext_dir })
    }
}

impl Drop for GnomeExtensionGuard {
    fn drop(&mut self) {
        // A never-activated install is pending until the next login; the
        // session manager stops the daemon during logout, *before* that
        // login happens, so tearing pending installs down at exit would
        // make the one-time setup never complete.  Keep it.
        if !LOADED.load(Relaxed) {
            return;
        }
        // Best effort, symmetric to install(): disable first so the Shell
        // unloads the extension before its files disappear.
        let _ = gnome_extensions(&["disable", EXTENSION_UUID]);
        let _ = fs::remove_dir_all(&self.ext_dir);
    }
}

/// Run the `gnome-extensions` CLI with `args`.
fn gnome_extensions(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("gnome-extensions").args(args).output()
}

/// Whether `ext_dir` already holds exactly the bundled contents.
fn files_current(ext_dir: &std::path::Path) -> bool {
    fn matches(path: &std::path::Path, content: &str) -> bool {
        fs::read_to_string(path).is_ok_and(|on_disk| on_disk == content)
    }

    matches(&ext_dir.join("metadata.json"), METADATA_CONTENT)
        && matches(&ext_dir.join("extension.js"), EXTENSION_CONTENT)
}

/// Enable the extension, best effort.
///
/// The CLI fails while the Shell has not scanned the new directory yet
/// (GNOME ≥ 45 scans only at session start); in that case the uuid is
/// appended to the `enabled-extensions` setting directly so the Shell
/// loads the extension at the next login.
fn enable_extension() {
    let known = gnome_extensions(&["enable", EXTENSION_UUID])
        .is_ok_and(|out| out.status.success());
    if !known {
        let _ = append_to_enabled_extensions();
    }
}

/// Append the uuid to `org.gnome.shell enabled-extensions` via `gsettings`,
/// preserving any extensions the user has enabled.
fn append_to_enabled_extensions() -> std::io::Result<()> {
    let current = Command::new("gsettings")
        .args(["get", ENABLED_EXTENSIONS_SCHEMA, ENABLED_EXTENSIONS_KEY])
        .output()?;
    if !current.status.success() {
        return Ok(());
    }
    let current = String::from_utf8_lossy(&current.stdout);
    let updated = enabled_extensions_add(&current, EXTENSION_UUID);
    if updated.is_none() {
        return Ok(()); // already listed
    }
    let _ = Command::new("gsettings")
        .args([
            "set",
            ENABLED_EXTENSIONS_SCHEMA,
            ENABLED_EXTENSIONS_KEY,
            &updated.unwrap(),
        ])
        .output();
    Ok(())
}

/// Compute the new GSettings value for `enabled-extensions` with `uuid`
/// appended to the GVariant list `list_text` (as printed by `gsettings
/// get`, e.g. `@as ['a', 'b']`).  Returns `None` when `uuid` is already
/// present.
fn enabled_extensions_add(list_text: &str, uuid: &str) -> Option<String> {
    // Strip the `@as ` type annotation and the enclosing brackets.
    let inner = list_text
        .trim()
        .split_once('[')
        .map(|(_, rest)| rest.trim_end().trim_end_matches(']'))
        .unwrap_or("");
    let mut uuids: Vec<&str> = if inner.trim().is_empty() {
        Vec::new()
    } else {
        inner
            .split(',')
            .map(|s| s.trim().trim_matches('\''))
            .collect()
    };
    if uuids.contains(&uuid) {
        return None;
    }
    uuids.push(uuid);
    let items: Vec<String> =
        uuids.into_iter().map(|u| format!("'{u}'")).collect();
    Some(format!("[{}]", items.join(", ")))
}

/// Install the extension on the first call; afterwards this is a no-op.
///
/// Retries while the guard slot is empty, so a daemon that started before
/// the session bus came up still succeeds once the bus is available.
fn ensure_installed() -> bool {
    // Fast path for steady-state queries.
    if INSTALLED.load(Relaxed) {
        return true;
    }
    // A poisoned lock only means an earlier op panicked after the
    // cleanup-relevant work, so continuing is safe.
    let mut slot = GUARD.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_some() {
        return true;
    }
    let guard = match GnomeExtensionGuard::install() {
        Ok(guard) => guard,
        Err(_) => return false,
    };
    ATEXIT_REGISTERED.call_once(|| {
        // SAFETY: registering an atexit handler is trivially safe; the
        // handler only touches `GUARD` (see `uninstall_extension`).
        unsafe { libc::atexit(uninstall_extension) };
    });
    *slot = Some(guard);
    INSTALLED.store(true, Relaxed);
    true
}

/// `atexit` handler: drops the guard on normal process exit.  Must never
/// panic — panicking inside an atexit handler aborts the process.
extern "C" fn uninstall_extension() {
    let mut slot = match GUARD.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    drop(slot.take());
}

/// Query the active window on GNOME through the Shell extension.
///
/// The extension itself is installed once per process by
/// [`ensure_installed`]; each query is a plain `GetActiveWindow` D-Bus
/// call.  Only the very first query may wait (up to [`LOAD_TIMEOUT`],
/// once) for the Shell to finish loading the extension.  Returns the
/// `.desktop` application id (without the `.desktop` suffix, matching
/// [`super::super::apps::resolve_process_app_id`]), or an empty string
/// when GNOME Shell is not running, the extension is not loaded (yet), or
/// no window has focus.
pub(super) fn query_gnome() -> String {
    use zbus::blocking::Connection;

    if !ensure_installed() {
        return String::new();
    }
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
            // One-time warm-up: the extension may have just been enabled
            // for a directory the Shell already knows, and loading is
            // asynchronous.  Outside a GNOME session there is nothing to
            // wait for.
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

    use super::{
        EXTENSION_UUID, LOAD_TIMEOUT, enabled_extensions_add, parse_app_id,
        query_gnome,
    };

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

    // -----------------------------------------------------------------------
    // enabled_extensions_add (gsettings list surgery)
    // -----------------------------------------------------------------------

    #[test]
    fn appends_to_empty_extension_list() {
        assert_eq!(
            enabled_extensions_add("@as []", "a@b.c").as_deref(),
            Some("['a@b.c']")
        );
    }

    #[test]
    fn appends_preserving_existing_entries() {
        assert_eq!(
            enabled_extensions_add("@as ['x@y.z', 'p@q.w']", "a@b.c")
                .as_deref(),
            Some("['x@y.z', 'p@q.w', 'a@b.c']")
        );
    }

    #[test]
    fn idempotent_when_already_enabled() {
        assert!(
            enabled_extensions_add("@as ['x@y.z']", "x@y.z").is_none(),
            "re-enabling must not rewrite the setting"
        );
    }

    #[test]
    fn round_trips_our_own_uuid() {
        let updated =
            enabled_extensions_add("@as []", EXTENSION_UUID).unwrap();
        assert!(updated.contains(EXTENSION_UUID));
        assert!(
            enabled_extensions_add(&format!("@as {updated}"), EXTENSION_UUID)
                .is_none()
        );
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
    fn repeat_queries_are_fast_once_installed() {
        // Installation and the one-time load wait happen at most once per
        // process; further calls must be plain D-Bus round-trips.
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
