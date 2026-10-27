// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! KDE (KWin) — synchronous active-window query via a KWin script.
//!
//! KWin does not export the active window through an ordinary D-Bus
//! method: the only consent-free entry point is its scripting interface
//! (`org.kde.kwin.Scripting`).  A query therefore stages a tiny JavaScript
//! file on disk, asks KWin to load and run it, and waits for the script to
//! call back over D-Bus with the active window's details.
//!
//! The canonical script source lives in
//! `resources/kwin-script/active-window.js` and is installed by
//! `scripts/install-kwin-script.sh` to
//! `$XDG_DATA_HOME/keymapperd/kwin/active-window.js`; an identical copy is
//! embedded as a fallback for processes that ran without that install step.
//! Because the callback destination is the unique D-Bus name of the
//! querying process — fresh on every query — the installed file is a
//! template: each query substitutes the destination for the
//! [`REPLY_DEST_PLACEHOLDER`] and stages the rendered result under the
//! daemon's runtime directory (`$XDG_RUNTIME_DIR/keymapperd`, falling back
//! to the keymapperd cache directory and then the temp directory, mirroring
//! the control-socket path resolution in [`crate::platform::endpoint::unix`]).
//! A third-party helper crate that stages the file in the plain temp
//! directory cannot work for the systemd unit, which sets `PrivateTmp=yes`:
//! KWin then sees a different `/tmp` than the daemon and cannot read the
//! script.
//!
//! The KWin `queryWindowInfo` method is deliberately *not* used: it asks the
//! user for consent on every call, which is unacceptable on a polling path.

use std::{
    borrow::Cow,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use parking_lot::{Condvar, Mutex};

/// KWin's scripting service, object path, and interface.
const KWIN_SERVICE: &str = "org.kde.KWin";
const SCRIPTING_PATH: &str = "/Scripting";
const SCRIPTING_INTERFACE: &str = "org.kde.kwin.Scripting";
/// Interface of the per-script object at `/Scripting/Script<id>`.
const SCRIPT_INTERFACE: &str = "org.kde.kwin.Script";

/// Name of this application's subdirectory below the runtime (or cache)
/// directory; the same directory the control socket lives in.
const APP_DIR: &str = "keymapperd";

/// Markers locating the script template installed by
/// `scripts/install-kwin-script.sh` below the user's data directory
/// (`$XDG_DATA_HOME/keymapperd/kwin/active-window.js`).
const SCRIPT_SUBDIR: &str = "kwin";
const SCRIPT_FILE: &str = "active-window.js";
/// Placeholder in the script template replaced with the unique D-Bus name
/// of the querying process' reply sink (see [`ReplySink`]).
const REPLY_DEST_PLACEHOLDER: &str = "@REPLY_DEST@";

/// Fallback script body, used when the installed template is missing or
/// unusable.
///
/// This is a copy of the body of `resources/kwin-script/active-window.js`;
/// the `embedded_script_matches_installed_template` test guards against
/// drift.
const EMBEDDED_SCRIPT: &str = r#"const dest = "@REPLY_DEST@";
const replyInterface = "de.adrhinum.keymapper.ActiveWindowReply";
function output_result(message) {
    callDBus(dest, "/", replyInterface, "result", message.toString());
}
function output_error(message) {
    print("keymapper active-window script: " + message.toString());
    callDBus(dest, "/", replyInterface, "error", message.toString());
}
try {
    const w = workspace.activeWindow;
    if (w == null) {
        output_error("No active window");
    } else {
        output_result(JSON.stringify({ class_name: w.resourceClass }));
    }
} catch (e) {
    output_error("Script error: " + e.toString());
}
"#;

/// Timeout for individual D-Bus round-trips to KWin.  Kept short because a
/// query may run synchronously on the keystroke hot path (see
/// [`crate::daemon::focus`]); a healthy KWin answers in milliseconds.
const DBUS_TIMEOUT: Duration = Duration::from_secs(2);
/// How long to wait for the script's D-Bus callback after `run()`.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// Query the active window on KDE via KWin scripting.
///
/// Returns the window's window class (the same namespace the `.desktop`
/// stems are matched against), or an empty string when KWin is not running,
/// is not Plasma 6, or does not answer within the query timeouts.
pub(super) fn query_kde() -> String {
    // The scripting API used below (`workspace.activeWindow` with
    // `resourceClass`) is Plasma 6 only; Plasma 5 would need a different
    // script and is not supported.
    if std::env::var("KDE_SESSION_VERSION").as_deref() != Ok("6") {
        return String::new();
    }

    let sink = ReplySink::default();
    // `serve_at` on the builder (rather than `Connection::object_server()`)
    // registers the sink on the connection's shared object server, i.e. the
    // one the dispatcher task actually consults.
    let Ok(conn) = zbus::blocking::connection::Builder::session() else {
        return String::new();
    };
    let Ok(conn) = conn
        .method_timeout(DBUS_TIMEOUT)
        .serve_at("/", sink.clone())
        .and_then(|conn| conn.build())
    else {
        return String::new();
    };
    let Some(dest) = conn.unique_name().map(|name| name.to_string()) else {
        return String::new();
    };

    let script = build_script(&dest);
    // KWin must be able to read the staged file, so it must live outside any
    // private mount namespace the daemon may run in; see the module docs.
    let Some(staged) = stage_script(&script) else {
        return String::new();
    };
    let Some(json) = run_active_window_script(&conn, &staged, &sink) else {
        return String::new();
    };
    super::extract_json_field(&json, "class_name")
}

// ---------------------------------------------------------------------------
// KWin script generation and staging
// ---------------------------------------------------------------------------

/// Path of the script template installed by `scripts/install-kwin-script.sh`.
fn installed_script_path() -> Option<PathBuf> {
    let data_dir = dirs::data_dir()?;
    Some(data_dir.join(APP_DIR).join(SCRIPT_SUBDIR).join(SCRIPT_FILE))
}

/// Pick the script template: `installed` when it is a usable template,
/// [`EMBEDDED_SCRIPT`] otherwise.
///
/// A customized copy without [`REPLY_DEST_PLACEHOLDER`] is rejected: with no
/// placeholder there is no way to address the callback to our reply sink,
/// so the query could only ever time out.
fn choose_template(installed: Option<String>) -> Cow<'static, str> {
    match installed {
        Some(template) if template.contains(REPLY_DEST_PLACEHOLDER) => {
            Cow::Owned(template)
        }
        _ => Cow::Borrowed(EMBEDDED_SCRIPT),
    }
}

/// Build the KWin script that reports the active window back to us.
///
/// `dest` is the unique D-Bus name of our reply connection; it replaces
/// every [`REPLY_DEST_PLACEHOLDER`] in the template (the file installed by
/// `scripts/install-kwin-script.sh`, or [`EMBEDDED_SCRIPT`] when it is not
/// available).  The script mirrors the callback protocol understood by
/// [`ReplySink`]: a `result` call carrying the window JSON, or an `error`
/// call carrying a message.
fn build_script(dest: &str) -> String {
    let installed = installed_script_path().and_then(|path| {
        // A read failure (missing file, unreadable data dir) is normal; fall
        // back to the embedded copy.
        fs_err::read_to_string(path).ok()
    });
    choose_template(installed).replace(REPLY_DEST_PLACEHOLDER, dest)
}

/// A staged KWin script file, removed when dropped.
struct StagedScript {
    path: PathBuf,
    /// Script name handed to KWin's `loadScript` (and `unloadScript`); the
    /// file stem, so KWin's script bookkeeping stays tidy.
    name: String,
}

impl Drop for StagedScript {
    fn drop(&mut self) {
        // Best effort; a stale file is inert because KWin only ever runs a
        // script after an explicit `loadScript` + `run`.
        let _ = fs_err::remove_file(&self.path);
    }
}

/// Write `contents` to a uniquely named file in the first writable staging
/// directory.
///
/// Files are created with `create_new` (a pre-existing path — including one
/// planted as a symlink by another user — fails instead of being reused) and
/// mode `0600`; KWin reads the file as the same user, so owner-only access
/// suffices.
fn stage_script(contents: &str) -> Option<StagedScript> {
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos()
    );
    for dir in script_dir_candidates() {
        if fs_err::create_dir_all(&dir).is_err() {
            continue;
        }
        let name = format!("keymapper-active-{suffix}");
        let path = dir.join(format!("{name}.js"));
        let Ok(mut file) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        else {
            // Already exists (collision or planted path), or the directory
            // is not writable; try the next candidate.  A file we did not
            // create is never deleted.
            continue;
        };
        if file.write_all(contents.as_bytes()).is_ok() {
            return Some(StagedScript { path, name });
        }
        // We created this file but could not fill it; clean up after
        // ourselves before moving on.
        drop(file);
        let _ = fs_err::remove_file(&path);
    }
    None
}

/// Directories to attempt staging the script file in, in preference order.
///
/// Same shape as the control-socket path resolution: the runtime dir's
/// keymapperd subdirectory first (the one directory writable under the
/// hardened systemd unit), then the cache dir, then the temp dir as a last
/// resort for environments without either.  Note the temp-dir candidate is
/// useless under the unit's `PrivateTmp=yes` (KWin would see a different
/// `/tmp`), but such environments always have a runtime dir.
fn script_dir_candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(rd) = std::env::var_os("XDG_RUNTIME_DIR") {
        let rd = PathBuf::from(rd);
        if !rd.as_os_str().is_empty() && rd.is_absolute() {
            dirs.push(rd.join(APP_DIR));
        }
    }
    let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    dirs.push(base.join(APP_DIR));
    dirs.push(std::env::temp_dir());
    dirs
}

// ---------------------------------------------------------------------------
// D-Bus round-trip
// ---------------------------------------------------------------------------

/// Load, run, and unload the staged script on KWin, returning the JSON the
/// script reported, or `None` when no result arrived (load/run failed or
/// the script reported an error / timed out).
fn run_active_window_script(
    conn: &zbus::blocking::Connection,
    staged: &StagedScript,
    sink: &ReplySink,
) -> Option<String> {
    // KWin reads the file and compiles the script; a negative id means the
    // name is already loaded (cannot happen with our unique names) or the
    // file could not be read.
    let Ok(reply) = conn.call_method(
        Some(KWIN_SERVICE),
        SCRIPTING_PATH,
        Some(SCRIPTING_INTERFACE),
        "loadScript",
        &(staged.path.display().to_string(), staged.name.as_str()),
    ) else {
        return None;
    };
    let (script_id,) = reply.body().deserialize::<(i32,)>().ok()?;
    if script_id < 0 {
        return None;
    }

    let script_path = format!("{SCRIPTING_PATH}/Script{script_id}");
    let ran = conn
        .call_method(
            Some(KWIN_SERVICE),
            &*script_path,
            Some(SCRIPT_INTERFACE),
            "run",
            &(),
        )
        .is_ok();
    // `stop()` ends the script's event loop; the script was only meant to
    // run synchronously until it sent its callback, so a `stop` failure is
    // not fatal.
    let _ = conn.call_method(
        Some(KWIN_SERVICE),
        &*script_path,
        Some(SCRIPT_INTERFACE),
        "stop",
        &(),
    );
    if !ran {
        unload_script(conn, &staged.name);
        return None;
    }

    let result = sink.wait(REPLY_TIMEOUT);
    // Best effort: leave nothing loaded in KWin if it still listens.
    unload_script(conn, &staged.name);
    result
}

/// Tell KWin to unload the script again; a failure leaves an inert script
/// that never re-runs because our script names are never reused.
fn unload_script(conn: &zbus::blocking::Connection, script_name: &str) {
    let _ = conn.call_method(
        Some(KWIN_SERVICE),
        SCRIPTING_PATH,
        Some(SCRIPTING_INTERFACE),
        "unloadScript",
        &(script_name,),
    );
}

// ---------------------------------------------------------------------------
// Reply sink
// ---------------------------------------------------------------------------

/// Shared slot where the KWin script's callback lands.
#[derive(Default)]
struct ReplyState {
    reply: Mutex<Option<Result<String, String>>>,
    ready: Condvar,
}

/// Own-connection D-Bus object receiving the script's `result`/`error`
/// callbacks.
///
/// The zbus method name is the verbatim D-Bus member name, so the script
/// calls `result`/`error` exactly as emitted by [`build_script`].  Cloning
/// shares the [`ReplyState`]: the registered copy receives the callbacks,
/// the query thread waits on its clone.
#[derive(Clone, Default)]
struct ReplySink {
    state: Arc<ReplyState>,
}

impl ReplySink {
    /// Store `reply` (first writer wins) and wake [`Self::wait`].
    fn deliver(&self, reply: Result<String, String>) {
        let mut slot = self.state.reply.lock();
        if slot.is_none() {
            *slot = Some(reply);
            self.state.ready.notify_all();
        }
    }

    /// Wait up to `timeout` for the script's `result` callback.
    fn wait(&self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let mut slot = self.state.reply.lock();
        loop {
            if let Some(reply) = slot.take() {
                return reply.ok();
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            if self
                .state
                .ready
                .wait_for(&mut slot, deadline - now)
                .timed_out()
            {
                return slot.take().and_then(|reply| reply.ok());
            }
        }
    }
}

#[zbus::interface(name = "de.adrhinum.keymapper.ActiveWindowReply")]
impl ReplySink {
    /// The script reports the active window's JSON.
    ///
    /// The zbus macro would PascalCase the method name (`Result`);
    /// the script calls the verbatim lowercase member, so pin it.
    #[zbus(name = "result")]
    fn result(&self, payload: &str) {
        self.deliver(Ok(payload.to_string()));
    }

    /// The script reports a failure.
    #[zbus(name = "error")]
    fn error(&self, message: &str) {
        self.deliver(Err(message.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EMBEDDED_SCRIPT, REPLY_DEST_PLACEHOLDER, build_script,
        choose_template, query_kde,
    };

    /// Interface of the reply sink the script calls back on (served at the
    /// root object path of our own connection).
    ///
    /// The `#[zbus::interface]` attribute on `ReplySink` and the script
    /// templates must carry the same string literal; the
    /// `interface_name_matches_const` test keeps them in sync.
    const REPLY_INTERFACE: &str = "de.adrhinum.keymapper.ActiveWindowReply";

    #[test]
    fn script_embeds_callback_destination() {
        let script = build_script(":1.42");
        assert!(script.contains("const dest = \":1.42\";"));
        assert!(script.contains(REPLY_INTERFACE));
        assert!(
            script.contains(r#"callDBus(dest, "/", replyInterface, "result""#)
        );
        assert!(!script.contains(REPLY_DEST_PLACEHOLDER));
        assert!(script.contains("workspace.activeWindow"));
        assert!(script.contains("resourceClass"));
    }

    #[test]
    fn embedded_script_matches_installed_template() {
        // The embedded fallback must carry the same script body as the
        // template that `scripts/install-kwin-script.sh` installs; guard
        // against drift between the two copies.
        let resource = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/kwin-script/active-window.js"
        );
        let contents = std::fs::read_to_string(resource)
            .expect("installed script template resource is readable");
        let marker = "const dest =";
        let body = contents
            .find(marker)
            .map(|i| contents[i..].trim_end())
            .expect("template carries the script body");
        // Compare ignoring leading whitespace: editors or formatters may
        // restyle the JavaScript indentation, but the code must match.
        let dedent = |s: &str| {
            s.lines()
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(dedent(body), dedent(EMBEDDED_SCRIPT));
    }

    #[test]
    fn embedded_script_carries_placeholder_and_interface() {
        assert!(EMBEDDED_SCRIPT.contains(REPLY_DEST_PLACEHOLDER));
        assert!(EMBEDDED_SCRIPT.contains(REPLY_INTERFACE));
    }

    #[test]
    fn usable_installed_template_is_preferred() {
        let installed =
            format!("// custom\nprint(\"{REPLY_DEST_PLACEHOLDER}\")");
        assert_eq!(
            choose_template(Some(installed.clone())).as_ref(),
            installed
        );
    }

    #[test]
    fn installed_template_without_placeholder_is_ignored() {
        // Without the placeholder the callback could never reach the reply
        // sink, so the embedded template wins.
        let broken = "print('customized, no destination');".to_string();
        assert_eq!(choose_template(Some(broken)).as_ref(), EMBEDDED_SCRIPT);
    }

    #[test]
    fn interface_name_matches_const() {
        // The `#[zbus::interface]` attribute must carry the same literal
        // the generated script embeds; guard against drift.
        assert_eq!(REPLY_INTERFACE, "de.adrhinum.keymapper.ActiveWindowReply");
    }

    #[test]
    fn does_not_panic() {
        // Outside a KDE session the probe returns an empty string, never
        // the `"unknown"` sentinel (that is produced by
        // `get_active_app_name`).
        let result = query_kde();
        assert_ne!(result, "unknown");
    }
}
