// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The OS-specific log destination behind the daemon's log sink.
//!
//! [`crate::daemon::logging`] owns the platform-agnostic parts of the daemon's
//! logging: the `log` facade, the runtime level gate, the ftlog line format,
//! the latency-stripping wrapper, and the panic hook. What it must *not* own
//! is where the bytes go and whether each line carries a timestamp — those are
//! per-OS decisions. Keeping them in `daemon::logging` meant OS `#[cfg]`
//! branches lived under `daemon/` (finding F9); this factory moves that
//! decision back out under `platform/`, one home for platform code.
//!
//! It is a `pub(crate)` facet like [`crate::platform::endpoint`]: only the
//! daemon consumes it, and the dependency arrow stays `daemon -> platform`.
//! The factory hands back a [`LogSink`] — the destination [`Write`] plus the
//! ftlog timestamp format — which the daemon then wraps in its shared format,
//! latency stripper, and level gate.
//!
//! The destination splits on Linux-vs-not rather than three ways, because the
//! two file platforms resolve an identical sink (the per-OS destination-path
//! differences live in the shared `file` child):
//!
//! - **Linux:** stderr, with an empty timestamp (systemd's journal timestamps
//!   each line, so the daemon must not duplicate it).
//! - **macOS / Windows:** a rotating file appender (daily rotation, 7-day
//!   retention) under the shared log directory, with ftlog's default
//!   `YYYY-MM-DD HH:MM:SS.mmm±HH` timestamp.

use std::io::Write;

use time::format_description::OwnedFormatItem;

/// The destination and timestamp policy the daemon's ftlog logger writes to.
///
/// Produced per platform by [`log_sink`]. `time_format` is `Some` to pin an
/// explicit ftlog timestamp format (Linux uses an empty one, since the journal
/// timestamps each line itself) and `None` to keep ftlog's default.
pub(crate) struct LogSink {
    /// The destination ftlog writes formatted lines to.
    pub(crate) root: Box<dyn Write + Send>,
    /// The ftlog timestamp format, or `None` for ftlog's default.
    pub(crate) time_format: Option<OwnedFormatItem>,
}

/// The file-logging platforms (macOS and Windows) share one rotating-file
/// destination; the split is Linux-vs-not rather than three-way, so both use a
/// single implementation instead of two identical copies.
#[cfg(not(target_os = "linux"))]
mod file;
#[cfg(target_os = "linux")]
mod linux;

/// Build this platform's log sink: its destination and timestamp policy.
///
/// Returns a descriptive error when the destination cannot be established
/// (for example, the log directory cannot be resolved or created).
/// [`crate::daemon::logging`] falls back to a stderr logger when this
/// fails, so the daemon never fails to start over logging.
#[cfg(not(target_os = "linux"))]
pub(crate) use file::log_sink;
/// Build this platform's log sink: its destination and timestamp policy.
///
/// Returns a descriptive error when the destination cannot be established
/// (for example, the log directory cannot be resolved).
/// [`crate::daemon::logging`] falls back to a stderr logger when this
/// fails, so the daemon never fails to start over logging.
#[cfg(target_os = "linux")]
pub(crate) use linux::log_sink;
