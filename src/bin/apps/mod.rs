// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Enumerates running applications and returns the exact `app_name` values
//! that keymapperd uses for config matching.  The returned names are produced
//! by the same platform APIs that `active-win-pos-rs` uses internally, so they
//! match 1:1 with what belongs in the `apps` field of `config.yaml`.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Return the sorted, deduplicated list of application names for all visible
/// windows owned by the current user.
///
/// These are the exact strings that should be used in the `apps` field of the
/// keymapperd configuration.
#[cfg(target_os = "macos")]
pub fn list_app_names() -> Vec<String> {
    macos::list_app_names()
}

#[cfg(target_os = "linux")]
pub fn list_app_names() -> Vec<String> {
    linux::list_app_names()
}

#[cfg(target_os = "windows")]
pub fn list_app_names() -> Vec<String> {
    windows::list_app_names()
}
