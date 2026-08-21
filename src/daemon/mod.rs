// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Daemon runtime: mapping cache, state management, and config hot-reload.

pub mod mapping_cache;
pub mod state;
pub mod watcher;

/// Environment variable naming the file the daemon touches once it is ready
/// to process events.
///
/// Set only by the e2e test harness so it can wait for the daemon to finish
/// initialisation (e.g. the DriverKit virtual HID driver loading on macOS)
/// before injecting keys.  When unset, [`signal_ready`] is a no-op, so this
/// has no effect in production.
pub const READY_FILE_ENV: &str = "KEYMAPPER_READY_FILE";

/// Touch the readiness file named by [`READY_FILE_ENV`], if set.
///
/// Called by each platform's `start_mapping` once the daemon is ready to
/// process events.  A no-op when the environment variable is unset, so it is
/// safe to call unconditionally.
pub fn signal_ready() {
    let Some(path) = std::env::var(READY_FILE_ENV).ok() else {
        return;
    };
    // Best-effort: a failure to signal readiness must not take down the
    // daemon.  The e2e harness treats a missing file as a timeout.
    let _ = fs_err::write(&path, "ready\n");
}
