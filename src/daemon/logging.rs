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
//! [`init`] also installs a panic hook that routes panics to `error!`
//! (message plus backtrace), so panics in background threads reach the
//! sink instead of a dead stdout.
//!
//! [`init`] is once per process: later calls are a no-op.

use std::{
    panic::{PanicHookInfo, set_hook},
    sync::atomic::{AtomicBool, Ordering},
};

use log::{LevelFilter, Log, Metadata, Record, error};

/// The process tag (syslog) / event source (Windows Event Log) name.
const LOG_TAG: &str = "keymapperd";

/// The maximum log level recorded. Info matches the visibility the daemon
/// had while it wrote to stdout/stderr directly.
const MAX_LEVEL: LevelFilter = LevelFilter::Info;

/// Guards against a second [`init`] call, which would fail the global
/// `log` logger installation.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

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
    install_sink();
    set_hook(Box::new(panic_hook));
}

/// Install the platform sink, falling back to stderr when it fails.
fn install_sink() {
    #[cfg(unix)]
    if let Err(reason) = install_syslog() {
        install_fallback(reason);
    }

    #[cfg(windows)]
    if let Err(reason) = install_eventlog() {
        install_fallback(reason);
    }
}

/// Install the unix syslog sink (RFC 3164 on `/dev/log`).
#[cfg(unix)]
fn install_syslog() -> Result<(), String> {
    let formatter = syslog::Formatter3164 {
        facility: syslog::Facility::LOG_USER,
        // Leave the hostname to the syslog daemon.
        hostname: None,
        process: LOG_TAG.into(),
        pid: std::process::id() as u32,
    };

    let logger = syslog::unix(formatter).map_err(|e| e.to_string())?;
    log::set_boxed_logger(Box::new(syslog::BasicLogger::new(logger)))
        .map_err(|e| e.to_string())?;
    log::set_max_level(MAX_LEVEL);
    Ok(())
}

/// Install the Windows Event Log sink.
#[cfg(windows)]
fn install_eventlog() -> Result<(), String> {
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

    // `init` opens the event source, installs the `log` backend writing to
    // the Application log, and sets the maximum level; it takes the
    // minimum `log::Level` the sink records.
    eventlog::init(LOG_TAG, log::Level::Info).map_err(|e| e.to_string())?;
    log::set_max_level(MAX_LEVEL);
    Ok(())
}

/// Install the stderr fallback sink.
///
/// Called when the platform sink is unavailable. The reason is printed
/// once before the logger is installed, because from then on the
/// fallback itself is the only output channel.
fn install_fallback(reason: String) {
    eprintln!(
        "{LOG_TAG}: platform log sink unavailable ({reason}); falling back \
         to stderr"
    );

    if log::set_boxed_logger(Box::new(StderrLogger)).is_err() {
        // A logger is already installed (should not happen); records are
        // routed there and there is nothing else to do.
        return;
    }
    log::set_max_level(MAX_LEVEL);
}

/// Minimal `log` backend that writes `[LEVEL] message` lines to stderr.
///
/// Only installed when the platform sink (syslog or the Windows Event
/// Log) could not be established.
struct StderrLogger;

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= MAX_LEVEL
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
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
    use log::Level;

    use super::*;

    /// Metadata for level-gating probes; no record is involved.
    fn metadata(level: Level) -> Metadata<'static> {
        Metadata::builder()
            .level(level)
            .target("keymapper::daemon::logging")
            .build()
    }

    /// [`init`] installs a usable global logger and is a no-op when called
    /// again.
    #[test]
    fn init_installs_a_logger() {
        init();
        init();

        assert!(log::logger().enabled(&metadata(Level::Info)));
    }

    /// [`StderrLogger`] gates on [`MAX_LEVEL`]: info and above are
    /// enabled, debug and below are not.
    #[test]
    fn stderr_logger_gates_levels() {
        let logger = StderrLogger;
        assert!(logger.enabled(&metadata(Level::Error)));
        assert!(logger.enabled(&metadata(Level::Warn)));
        assert!(logger.enabled(&metadata(Level::Info)));
        assert!(!logger.enabled(&metadata(Level::Debug)));
        assert!(!logger.enabled(&metadata(Level::Trace)));
    }
}
