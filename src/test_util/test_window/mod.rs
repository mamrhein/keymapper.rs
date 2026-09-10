// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Test-window helper for the e2e harness.
//!
//! Opens and focuses a window whose owning process resolves to a known app
//! name, so [`crate::common::app_identity::get_active_app_name`] is
//! deterministic during a test run.  The process keeps the window focused by
//! running a message loop until it is terminated (SIGTERM/SIGINT on unix, or
//! a hard kill on Windows).
//!
//! The resolved app name differs per platform:
//!
//! - **Windows** — the helper's executable file name (e.g.
//!   `keymapper_testwindow.exe`).
//! - **Linux** — the `.desktop` application id of a fixture that maps the
//!   helper's executable name (e.g. `keymapper.testwindow`).
//! - **macOS** — the CoreGraphics window owner name (the helper's process
//!   name).
//!
//! The harness queries the live active-app name after focusing the window and
//! matches config rules against that value, so no platform-specific string is
//! hardcoded in the test logic.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Open and focus the test window, then block until a shutdown signal.
pub fn run() {
    #[cfg(target_os = "linux")]
    linux::run();

    #[cfg(target_os = "macos")]
    macos::run();

    #[cfg(target_os = "windows")]
    windows::run();

    // The daemon itself only builds for the three platforms above, so the
    // test window is useless anywhere else; fail loudly instead of exiting
    // silently.
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    )))]
    {
        eprintln!(
            "error: keymapper_testwindow is not supported on this platform"
        );
        std::process::exit(1);
    }
}
