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
//! All daemon-side output goes through the `log` facade. The sink is an
//! ftlog logger installed once per process by [`init`], writing to a
//! platform-specific destination:
//!
//! - **Linux:** stderr, without an embedded timestamp (the journal adds one).
//!   systemd forwards it to the journal.
//! - **macOS:** a rotating file at `~/Library/Logs/keymapper/keymapperd.log`
//!   (daily rotation, 7-day retention).
//! - **Windows:** a rotating file at
//!   `%LOCALAPPDATA%\keymapperd\logs\keymapperd.log` (daily rotation, 7-day
//!   retention).
//!
//! The line format is `{timestamp} {LEVEL} {target}: {message}`, where the
//! timestamp (ftlog's default `YYYY-MM-DD HH:MM:SS.mmm±HH`) appears only on
//! the file platforms. ftlog hard-codes a `{latency}ms` field into every
//! line and offers no public API to remove it, so the destination is wrapped
//! in [`LatencyStrippingWriter`], which strips that field from each line.
//!
//! If the ftlog logger cannot be built — the log directory is not writable,
//! for example — a minimal stderr fallback logger ([`StderrLogger`]) is
//! installed instead and a one-time notice is printed. The daemon must never
//! fail to start over logging; on Linux and macOS the stderr fallback still
//! reaches the system log.
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

#[cfg(not(target_os = "linux"))]
use std::path::PathBuf;
use std::{
    borrow::Cow,
    fmt,
    io::{self, Write},
    panic::{PanicHookInfo, set_hook},
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

/// Re-export the `log` level type so callers (the CLI, the control
/// socket) can name it without adding a direct `log` dependency.
pub use log::LevelFilter;
use log::{Level, Log, Metadata, Record, error, info};
#[cfg(target_os = "linux")]
use time::format_description::OwnedFormatItem;

/// The process name used in the fallback notice.
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
    info!("Initial log level: {initial:?}");
}

/// Change the runtime log level without a restart.
///
/// Stores *level* in the gate's [`CURRENT_LEVEL`]. The level-gating logger
/// reads that atomic on every record, so the change takes effect immediately
/// for every thread. The `log` facade stays pinned to [`LevelFilter::Trace`],
/// so no `set_max_level` call is needed. The control socket's accept thread
/// calls this; the environment variable only seeds the initial level.
pub fn set_level(level: LevelFilter) {
    CURRENT_LEVEL.store(level_to_u8(level), Ordering::SeqCst);
    info!("Log level changed to: {level:?}");
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

/// Build the platform sink (ftlog on every platform) as an inner sink. The
/// stderr fallback is applied by the caller when this fails.
fn install_platform_sink() -> Result<Box<dyn Log + Send + Sync>, String> {
    install_ftlog()
}

/// Build the ftlog sink: the platform destination, wrapped in the latency
/// stripper, with the line format and (on Linux) the empty timestamp.
fn install_ftlog() -> Result<Box<dyn Log + Send + Sync>, String> {
    let root = platform_root()?;
    build_ftlog(root)
        .map(|logger| Box::new(logger) as Box<dyn Log + Send + Sync>)
}

/// The destination the ftlog logger writes to, per platform.
///
/// - **Linux:** stderr; systemd forwards it to the journal.
/// - **macOS / Windows:** a rotating file appender (daily rotation, 7-day
///   retention). The parent directory is created if absent because ftlog's
///   appender does not create it.
fn platform_root() -> Result<Box<dyn Write + Send>, String> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(io::stderr()))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let path = log_file_path()?;
        if let Some(parent) = path.parent() {
            fs_err::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        let appender = ftlog::appender::FileAppender::builder()
            .path(&path)
            .rotate(ftlog::appender::Period::Day)
            .expire(ftlog::appender::Duration::days(7))
            .build();
        Ok(Box::new(appender))
    }
}

/// The log file path on the file-based platforms (macOS, Windows).
#[cfg(not(target_os = "linux"))]
fn log_file_path() -> Result<PathBuf, String> {
    let dir = if cfg!(windows) {
        // The config lives in %APPDATA%; the logs go to %LOCALAPPDATA%.
        dirs::data_local_dir()
            .ok_or_else(|| {
                "no local data directory (LOCALAPPDATA) available".to_string()
            })?
            .join("keymapperd")
            .join("logs")
    } else {
        dirs::home_dir()
            .ok_or_else(|| "no home directory available".to_string())?
            .join("Library")
            .join("Logs")
            .join("keymapper")
    };
    // Name the file after the running process so keymapperd and virtkbdd log
    // to separate files.  Fall back to the historical name when the executable
    // path cannot be resolved.
    let file_name = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.file_stem()
                .map(|stem| format!("{}.log", stem.to_string_lossy()))
        })
        .unwrap_or_else(|| "keymapperd.log".to_string());
    Ok(dir.join(file_name))
}

/// Build the ftlog logger writing to *root*.
///
/// The logger's own level is pinned to [`LevelFilter::Trace`] so it never
/// filters on its own; the gate's atomic is the single level gate. On Linux
/// the timestamp is empty (the journal adds one); on the file platforms
/// ftlog's default `YYYY-MM-DD HH:MM:SS.mmm±HH` is used.
fn build_ftlog(
    root: impl Write + Send + 'static,
) -> Result<ftlog::Logger, String> {
    let builder = ftlog::builder()
        .max_log_level(LevelFilter::Trace)
        .format(FtLogFormat);

    #[cfg(target_os = "linux")]
    let builder = builder.time_format(empty_time_format());

    builder
        .root(LatencyStrippingWriter { inner: root })
        .build()
        .map_err(|e| e.to_string())
}

/// An empty `time` format description, so ftlog writes no timestamp on
/// Linux (the journal adds one).
#[cfg(target_os = "linux")]
fn empty_time_format() -> OwnedFormatItem {
    // An empty description always parses (to an empty compound item that
    // formats to the empty string).
    time::format_description::parse_owned::<1>("")
        .expect("an empty format description always parses")
}

/// The line body ftlog writes for every record: `{LEVEL} {target}: {message}`.
///
/// The timestamp (file platforms only) is ftlog's own field, written before
/// the body; the hard-coded `{latency}ms` field is stripped by
/// [`LatencyStrippingWriter`]. Together they produce the locked format
/// `{timestamp} {LEVEL} {target}: {message}`.
struct FtLogFormat;

impl ftlog::FtLogFormat for FtLogFormat {
    fn msg(&self, record: &Record) -> Box<dyn Send + Sync + fmt::Display> {
        Box::new(FtLogMessage {
            level: record.level(),
            // The standard `log` macros set the module path as a static, so
            // the common case borrows instead of allocating.
            target: record
                .module_path_static()
                .map(Cow::Borrowed)
                .unwrap_or_else(|| Cow::Owned(record.target().to_owned())),
            args: record
                .args()
                .as_str()
                .map(Cow::Borrowed)
                .unwrap_or_else(|| Cow::Owned(format!("{}", record.args()))),
        })
    }
}

/// The formatted body of one log line (see [`FtLogFormat`]).
struct FtLogMessage {
    level: Level,
    target: Cow<'static, str>,
    args: Cow<'static, str>,
}

impl fmt::Display for FtLogMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}: {}", self.level, self.target, self.args)
    }
}

/// A `Write` wrapper that strips ftlog's hard-coded `{latency}ms` field from
/// each line.
///
/// ftlog writes every line as `"{timestamp} {latency}ms {body}\n"` in a
/// single `write` call and offers no option to drop the latency field, so it
/// is removed here: the first ` {N}ms ` (space, digits, `ms`, space) is
/// always the latency field — it sits right after the possibly empty
/// timestamp, before the level — so matching the first occurrence is safe
/// even when the message body itself contains a ` {N}ms ` sequence. On Linux
/// the strip also removes the leading space the empty timestamp leaves.
struct LatencyStrippingWriter<W: Write> {
    inner: W,
}

impl<W: Write> Write for LatencyStrippingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match find_latency_field(buf) {
            Some((start, end)) => {
                self.inner.write_all(&buf[..start])?;
                self.inner.write_all(&buf[end..])?;
            }
            None => {
                self.inner.write_all(buf)?;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Find the first ` {N}ms ` field (space, one or more ASCII digits, `ms`,
/// space) in *line*; the returned bounds span the field including both
/// spaces.
fn find_latency_field(line: &[u8]) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(rel) = line[from..].iter().position(|&b| b == b' ') {
        let start = from + rel;
        let rest = &line[start + 1..];
        let digits = rest.iter().take_while(|&&b| b.is_ascii_digit()).count();
        if digits > 0 && rest[digits..].starts_with(b"ms ") {
            return Some((start, start + 1 + digits + 3));
        }
        from = start + 1;
    }
    None
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

/// The level-gating `log` backend installed as the global logger.
///
/// [`enabled`] reads the runtime [`CURRENT_LEVEL`] — the single gate for
/// every record — and forwards to the ftlog sink underneath. Because the
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
/// Only installed (as the inner sink) when the ftlog logger could not be
/// built. It forwards every record the level gate admits; the gate is the
/// single level gate.
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

    // A separate record so a long backtrace cannot crowd out the panic
    // message itself.
    error!(
        "Panic backtrace:\n{}",
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

    /// [`set_level`] updates the gate in place, so a live change takes effect
    /// for the already-installed logger without a restart.
    #[test]
    fn set_level_changes_the_installed_gate() {
        let _guard = LEVEL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        init();

        set_level(LevelFilter::Debug);
        let logger = log::logger();
        assert!(logger.enabled(&metadata(Level::Debug)));
        assert!(!logger.enabled(&metadata(Level::Trace)));

        set_level(LevelFilter::Info);
        assert!(!logger.enabled(&metadata(Level::Debug)));
        assert!(logger.enabled(&metadata(Level::Info)));
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

    /// The ftlog sink receives records through the gate and writes the
    /// locked line format — `{LEVEL} {target}: {message}`, with no ftlog
    /// latency field — to its destination.
    #[test]
    fn ftlog_sink_writes_the_locked_format() {
        let _guard = LEVEL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("probe.log");
        let appender =
            ftlog::appender::FileAppender::builder().path(&path).build();

        let logger = build_ftlog(appender).expect("ftlog logger");
        install_gate(Box::new(logger));

        CURRENT_LEVEL.store(level_to_u8(LevelFilter::Info), Ordering::SeqCst);
        info!("ftlog sink probe");
        // ftlog writes from a background thread; the synchronous flush
        // blocks until the record has reached the file.
        log::logger().flush();

        let content = fs_err::read_to_string(&path).expect("log file");
        assert!(
            content.contains(
                "INFO keymapper::daemon::logging::tests: ftlog sink probe"
            ),
            "unexpected line format: {content:?}"
        );
        assert!(
            !content.contains("ms "),
            "latency field not stripped: {content:?}"
        );
    }
}
