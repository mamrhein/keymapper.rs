// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! E2e-only test hooks.
//!
//! This module is the single home for the test-specific behaviour compiled
//! into the daemon.  The whole module sits behind the `e2e` cargo feature,
//! so none of it is present in production builds at all.  Within the module
//! each hook is additionally gated on an environment variable that only the
//! e2e harness sets:
//!
//! - [`signal_ready`]: the daemon touches a readiness file once it can process
//!   events; the harness waits for it before injecting keys (e.g. the
//!   DriverKit virtual HID driver loading on macOS).
//! - [`active_app_name`]: pins the active application name, because the
//!   headless e2e monitor has no window and can never become active.
//!
//! If more hooks are needed, keep them here rather than scattering
//! env-gated branches through the production code paths.

use crate::common::app_identity;

/// Environment variable naming the file the daemon touches once it is ready
/// to process events.
pub const READY_FILE_ENV: &str = "KEYMAPPER_READY_FILE";

/// Environment variable that overrides the platform's active-app query.
pub const ACTIVE_APP_OVERRIDE_ENV: &str = "KEYMAPPER_ACTIVE_APP";

/// Touch the readiness file named by [`READY_FILE_ENV`], if set.
///
/// Injected into each platform's `start_mapping` by the daemon binary and
/// invoked once the daemon is ready to process events.  A no-op when the
/// environment variable is unset, so it is safe to pass unconditionally.
pub fn signal_ready() {
    let Some(path) = std::env::var(READY_FILE_ENV).ok() else {
        return;
    };
    // Best-effort: a failure to signal readiness must not take down the
    // daemon.  The e2e harness treats a missing file as a timeout.
    let _ = fs_err::write(&path, "ready\n");
}

/// Resolve the active application name, honoring the e2e override.
pub fn active_app_name() -> String {
    match std::env::var(ACTIVE_APP_OVERRIDE_ENV) {
        Ok(name) if !name.is_empty() => name,
        _ => app_identity::get_active_app_name(),
    }
}
