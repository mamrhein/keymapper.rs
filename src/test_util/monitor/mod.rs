// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Cross-platform keyboard event monitor for e2e testing.
//!
//! Each supported platform captures the daemon's output directly, without a
//! window or keyboard-focus dependency: Linux grabs the daemon's uinput
//! output device, Windows installs a low-level keyboard hook that logs every
//! key reaching the session's hook chain (the daemon's outputs plus forwarded
//! passthroughs), and macOS seizes the daemon's Karabiner DriverKit virtual
//! keyboard.  Captured events are written to a file or to stdout for the e2e
//! test harness.

use std::sync::{Arc, atomic::AtomicBool};

use crate::common::hid_usage::HidUsage;

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(target_os = "windows")]
pub(crate) mod windows;
pub mod writer;

/// A single captured keyboard event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputEvent {
    /// Whether the key was pressed (true) or released (false).
    pub down: bool,
    /// The key that changed state.
    pub key: HidUsage,
}

/// Entry point for the monitor application.
///
/// Dispatches to the platform-specific capture backend (see the module
/// docs).  All backends are windowless and headless-friendly, and exit
/// cleanly on SIGTERM/SIGINT.
pub fn run(sink: writer::EventWriter) {
    #[cfg(target_os = "linux")]
    linux::run(sink);

    #[cfg(target_os = "macos")]
    macos::run(sink);

    #[cfg(target_os = "windows")]
    windows::run(sink);

    // The daemon itself only builds for the three platforms above, so the
    // monitor is useless anywhere else; fail loudly instead of exiting
    // silently.
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    )))]
    {
        eprintln!(
            "error: keymapper_monitor is not supported on this platform"
        );
        std::process::exit(1);
    }
}

/// Register unix signal handlers for graceful shutdown.
///
/// Returns an `AtomicBool` that is set to `true` when a shutdown signal
/// (SIGINT or SIGTERM) is received.
#[cfg(unix)]
pub fn register_signal_handlers() -> Arc<AtomicBool> {
    use signal_hook::{
        consts::signal::{SIGINT, SIGTERM},
        flag,
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    flag::register(SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    flag::register(SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");
    shutdown
}

/// No-op signal registration on Windows.
#[cfg(not(unix))]
pub fn register_signal_handlers() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
