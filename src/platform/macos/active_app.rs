// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Synchronous foreground application query via NSWorkspace.

use objc2_app_kit::NSWorkspace;

/// Synchronously query the current foreground application name.
///
/// Returns `"unknown"` if no application is in the foreground or the
/// query fails.
pub fn get_active_app_name() -> String {
    // NSWorkspace is a Foundation singleton that is safe to access from
    // any thread.  The subsequent calls only read immutable state from the
    // window server.
    let workspace = NSWorkspace::sharedWorkspace();

    // Extract the frontmost application outside the let-else to satisfy
    // Rust 2024 edition restrictions on unsafe blocks in pattern guards.
    let maybe_app = workspace.frontmostApplication();
    let Some(app) = maybe_app else {
        return "unknown".to_string();
    };

    // Prefer the localized display name; fall back to the bundle
    // identifier if the display name is unavailable.
    if let Some(name) = app.localizedName() {
        return name.to_string();
    }

    if let Some(bundle_id) = app.bundleIdentifier() {
        return bundle_id.to_string();
    }

    "unknown".to_string()
}
