// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Logging setup for the daemon.
//!
//! All daemon-side output goes through the `log` facade. The sink is
//! selected at compile time and installed once per process by [`init`]:
//!
//! - **unix (Linux + macOS):** RFC 3164 syslog on `/dev/log` with facility
//!   `LOG_USER`. On macOS `/dev/log` routes into unified logging, so one code
//!   path covers both platforms.
//! - **Windows:** the Windows Event Log (Application log, source
//!   `keymapperd`).
//!
//! If the platform sink cannot be established — `/dev/log` is absent on a
//! journald-only system, or the Windows event source could not be opened —
//! a minimal stderr fallback logger ([`StderrLogger`]) is installed instead
//! and a one-time notice is printed. The daemon must never fail to start
//! over logging; on a systemd-managed Linux install the stderr fallback
//! still reaches the journal.
//!
//! Whatever sink is chosen is wrapped in a level-gating logger
//! ([`LevelGateLogger`]) that is the single point at which records are
//! filtered by level. The gate's threshold is the runtime value
//! [`CURRENT_LEVEL`], seeded from the `KEYMAPPERD_LOG_LEVEL` environment
//! variable (default `info`) at [`init`] and changeable later without a
//! restart. The `log` facade itself is pinned to [`LevelFilter::Trace`] so
//! it never filters on its own; raising the level merely lets more records
//! through the *same* sink, it never redirects them, so the `info`
//! production path is byte-for-byte unchanged.
//!
//! [`init`] also installs a panic hook that routes panics to `error!`
//! (message plus backtrace), so panics in background threads reach the
//! sink instead of a dead stdout.
//!
//! [`init`] is once per process: later calls are a no-op.

use std::{
    panic::{PanicHookInfo, set_hook},
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

use log::{LevelFilter, Log, Metadata, Record, error};

/// The process tag (syslog) / event source (Windows Event Log) name.
const LOG_TAG: &str = "keymapperd";

/// Environment variable that seeds the initial log level.
///
/// Read once at [`init`], before any events flow. It accepts the `log` level
/// names (`error`, `warn`, `info`, `debug`, `trace`) and defaults to `info`
/// when unset or unrecognised. In production (service-manager mode) the
/// unit's environment supplies it; a later control socket becomes the
/// runtime setter for a live change.
const LOG_LEVEL_ENV: &str = "KEYMAPPERD_LOG_LEVEL";

/// The log level used when [`LOG_LEVEL_ENV`] is unset or unrecognised.
const DEFAULT_LEVEL: LevelFilter = LevelFilter::Info;

/// The runtime log-level gate.
///
/// Stores the discriminant of the active [`LevelFilter`] (see
/// [`level_to_u8`]). [`LevelGateLogger::enabled`] reads this, so it is the
/// single gate that decides which records reach the sink. Raising it merely
/// admits more records through the same sink; it never redirects them.
static CURRENT_LEVEL: AtomicU8 = AtomicU8::new(level_to_u8(DEFAULT_LEVEL));

/// Guards against a second [`init`] call, which would fail the global
/// `log` logger installation.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Encode a [`LevelFilter`] as its discriminant (0 = Off .. 5 = Trace).
const fn level_to_u8(level: LevelFilter) -> u8 {
    match level {
        LevelFilter::Off => 0,
        LevelFilter::Error => 1,
        LevelFilter::Warn => 2,
        LevelFilter::Info => 3,
        LevelFilter::Debug => 4,
        LevelFilter::Trace => 5,
    }
}

/// Decode a discriminant written by [`level_to_u8`] back to a
/// [`LevelFilter`]; anything out of range maps to the most verbose level.
const fn level_from_u8(value: u8) -> LevelFilter {
    match value {
        0 => LevelFilter::Off,
        1 => LevelFilter::Error,
        2 => LevelFilter::Warn,
        3 => LevelFilter::Info,
        4 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    }
}

/// Install the process-wide `log` backend and the panic hook.
///
/// This must be called before any other logging, i.e. as the first
/// statement of the daemon's `main`. It never fails: when the platform
/// sink is unavailable, the stderr fallback is installed instead.
pub fn init() {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        // The global logger can only be installed once; the first call
        // already took effect.
        return;
    }

    // Seed the initial level from the environment before any events flow.
    let initial =
        parse_log_level(std::env::var(LOG_LEVEL_ENV).ok().as_deref());
    CURRENT_LEVEL.store(level_to_u8(initial), Ordering::SeqCst);

    install_sink();
    set_hook(Box::new(panic_hook));
}

/// Parse the [`LOG_LEVEL_ENV`] value into a [`LevelFilter`].
///
/// Falls back to [`DEFAULT_LEVEL`] when the variable is unset, blank, or
/// does not name a known level, so a mistyped value degrades to `info`
/// rather than failing to start.
fn parse_log_level(value: Option<&str>) -> LevelFilter {
    value
        .and_then(|s| s.trim().parse::<LevelFilter>().ok())
        .unwrap_or(DEFAULT_LEVEL)
}

/// Build the platform sink, fall back to stderr when it is unavailable, and
/// install it wrapped in the level-gating logger.
fn install_sink() {
    let inner = install_platform_sink().unwrap_or_else(|reason| {
        eprintln!(
            "{LOG_TAG}: platform log sink unavailable ({reason}); falling \
             back to stderr"
        );
        Box::new(StderrLogger) as Box<dyn Log + Send + Sync>
    });

    install_gate(inner);
}

/// Build the platform sink (syslog on unix, the Windows Event Log on
/// Windows) as an inner sink. Sink selection is unchanged; the stderr
/// fallback is applied by the caller when this fails.
#[cfg(unix)]
fn install_platform_sink() -> Result<Box<dyn Log + Send + Sync>, String> {
    install_syslog()
}

#[cfg(windows)]
fn install_platform_sink() -> Result<Box<dyn Log + Send + Sync>, String> {
    install_eventlog()
}

/// Wrap *inner* in the level-gating logger and install it as the global
/// `log` backend.
///
/// The facade's own max level is pinned to [`LevelFilter::Trace`] so it
/// never filters; the gate's atomic read becomes the single level gate.
fn install_gate(inner: Box<dyn Log + Send + Sync>) {
    let gate = LevelGateLogger { inner };
    if log::set_boxed_logger(Box::new(gate)).is_err() {
        // A logger is already installed (should not happen; [`init`]
        // guards the first call). Records are routed there and there is
        // nothing else to do.
        return;
    }
    log::set_max_level(LevelFilter::Trace);
}

/// Install the unix syslog sink (RFC 3164 on `/dev/log`).
#[cfg(unix)]
fn install_syslog() -> Result<Box<dyn Log + Send + Sync>, String> {
    let formatter = syslog::Formatter3164 {
        facility: syslog::Facility::LOG_USER,
        // Leave the hostname to the syslog daemon.
        hostname: None,
        process: LOG_TAG.into(),
        pid: std::process::id() as u32,
    };

    let logger = syslog::unix(formatter).map_err(|e| e.to_string())?;
    Ok(Box::new(syslog::BasicLogger::new(logger)))
}

/// Install the Windows Event Log sink.
#[cfg(windows)]
fn install_eventlog() -> Result<Box<dyn Log + Send + Sync>, String> {
    // Registering the event source writes an HKLM key and requires
    // elevation. It only adds the event source's message-file entry, so a
    // failure is a one-time notice rather than a fatal error.
    if let Err(e) = eventlog::register(LOG_TAG) {
        eprintln!(
            "{LOG_TAG}: could not register the Windows event source ({e}). \
             Run `keymapper daemon start` once from an elevated prompt to \
             fix this."
        );
    }

    // Build the sink directly (not via `eventlog::init`, which installs its
    // own `log` backend and pins the level). The sink's own level is set to
    // `Trace` so it forwards everything the gate lets through; the gate's
    // atomic is the single level gate, which is what keeps debug records
    // flowing on Windows where the raw sink would otherwise drop them.
    let inner = eventlog::EventLog::new(LOG_TAG, log::Level::Trace)
        .map_err(|e| e.to_string())?;
    Ok(Box::new(inner))
}

/// The level-gating `log` backend installed as the global logger.
///
/// [`enabled`] reads the runtime [`CURRENT_LEVEL`] — the single gate for
/// every record — and forwards to the platform sink underneath. Because the
/// facade is pinned to `Trace`, a record reaches [`log`] only if it passes
/// this gate. Wrapping every sink in this way keeps all three platforms
/// identical and lets the level be raised at runtime without a restart.
struct LevelGateLogger {
    inner: Box<dyn Log + Send + Sync>,
}

impl Log for LevelGateLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let current = level_from_u8(CURRENT_LEVEL.load(Ordering::Relaxed));
        metadata.level() <= current
    }

    fn log(&self, record: &Record) {
        // Re-check the gate so a level lowered between the facade's check
        // and this call cannot leak a record through.
        if self.enabled(record.metadata()) {
            self.inner.log(record);
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// Minimal `log` backend that writes `[LEVEL] message` lines to stderr.
///
/// Only installed (as the inner sink) when the platform sink (syslog or the
/// Windows Event Log) could not be established. It forwards every record the
/// level gate admits; the gate is the single level gate.
struct StderrLogger;

impl Log for StderrLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        eprintln!("[{}] {}", record.level(), record.args());
    }

    fn flush(&self) {}
}

/// Panic hook routing panics to the installed sink.
///
/// The default hook writes to stderr, which is dead for a daemon whose
/// stdout/stderr go to a service manager or nowhere; routing through
/// `error!` puts panics — including those from background threads — into
/// the same log as everything else.
fn panic_hook(info: &PanicHookInfo) {
    let location = info
        .location()
        .map(|l| l.to_string())
        .unwrap_or_else(|| "unknown location".into());
    let message = panic_message(info);
    error!("panic at {location}: {message}");

    // A separate record: a long backtrace may exceed what a single syslog
    // datagram carries, and the message itself should survive that.
    error!(
        "panic backtrace:\n{}",
        std::backtrace::Backtrace::force_capture()
    );
}

/// The panic payload as a string, for `&str` and `String` payloads.
fn panic_message(info: &PanicHookInfo) -> String {
    if let Some(msg) = info.payload().downcast_ref::<&str>() {
        (*msg).to_string()
    } else if let Some(msg) = info.payload().downcast_ref::<String>() {
        msg.clone()
    } else {
        "panic with non-string payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use log::Level;

    use super::*;

    /// Serializes the tests that read or write the process-wide
    /// [`CURRENT_LEVEL`], so the level-gating assertions never observe a
    /// level another test is mid-change on.
    static LEVEL_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Metadata for level-gating probes; no record is involved.
    fn metadata(level: Level) -> Metadata<'static> {
        Metadata::builder()
            .level(level)
            .target("keymapper::daemon::logging")
            .build()
    }

    /// [`init`] installs a usable global logger (the level gate) and is a
    /// no-op when called again. The default level is `info`, so info and
    /// above are enabled and debug and below are gated out until the level
    /// is raised at runtime.
    #[test]
    fn init_installs_a_logger() {
        let _guard = LEVEL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        init();
        init();

        let logger = log::logger();
        assert!(logger.enabled(&metadata(Level::Error)));
        assert!(logger.enabled(&metadata(Level::Warn)));
        assert!(logger.enabled(&metadata(Level::Info)));
        assert!(!logger.enabled(&metadata(Level::Debug)));
        assert!(!logger.enabled(&metadata(Level::Trace)));
    }

    /// The level gate admits every record at or below the runtime
    /// [`CURRENT_LEVEL`] and drops the rest, so raising the level lets more
    /// records through the same sink.
    #[test]
    fn level_gate_gates_levels() {
        let _guard = LEVEL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Establish the logger first so init's one-time level store has
        // completed before the gate is driven.
        init();

        let gate = LevelGateLogger {
            inner: Box::new(StderrLogger),
        };

        CURRENT_LEVEL.store(level_to_u8(LevelFilter::Debug), Ordering::SeqCst);
        assert!(gate.enabled(&metadata(Level::Error)));
        assert!(gate.enabled(&metadata(Level::Warn)));
        assert!(gate.enabled(&metadata(Level::Info)));
        assert!(gate.enabled(&metadata(Level::Debug)));
        assert!(!gate.enabled(&metadata(Level::Trace)));

        CURRENT_LEVEL.store(level_to_u8(LevelFilter::Off), Ordering::SeqCst);
        assert!(!gate.enabled(&metadata(Level::Error)));

        // Restore the default so the other test observes it.
        CURRENT_LEVEL.store(level_to_u8(DEFAULT_LEVEL), Ordering::SeqCst);
    }

    /// The level seed parses the env var (case-insensitive, matching the
    /// `log` level names) and falls back to `info` when it is unset, blank,
    /// or unrecognised.
    #[test]
    fn parse_log_level_defaults_and_parses() {
        assert_eq!(parse_log_level(None), DEFAULT_LEVEL);
        assert_eq!(parse_log_level(Some("")), DEFAULT_LEVEL);
        assert_eq!(parse_log_level(Some("  ")), DEFAULT_LEVEL);
        assert_eq!(parse_log_level(Some("error")), LevelFilter::Error);
        assert_eq!(parse_log_level(Some("Debug")), LevelFilter::Debug);
        assert_eq!(parse_log_level(Some("TRACE")), LevelFilter::Trace);
        assert_eq!(parse_log_level(Some("bogus")), DEFAULT_LEVEL);
    }
}
