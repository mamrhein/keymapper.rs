// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Application identity queries shared by the daemon and the CLI.
//!
//! Both entry points produce the application names that keymapperd matches
//! rules against:
//!
//! - [`get_active_app_name`] returns the name of the current foreground
//!   application, used by the daemon for rule matching.
//! - [`list_app_names`] returns the names of all visible applications, printed
//!   by `keymapper appnames`.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Synchronously query the name of the current foreground application.
///
/// Returns `"unknown"` if no application is in the foreground or the query
/// fails.
#[cfg(target_os = "linux")]
pub fn get_active_app_name() -> String {
    linux::get_active_app_name()
}

#[cfg(target_os = "macos")]
pub fn get_active_app_name() -> String {
    macos::get_active_app_name()
}

#[cfg(target_os = "windows")]
pub fn get_active_app_name() -> String {
    windows::get_active_app_name()
}

/// Return the sorted, deduplicated list of application names for all visible
/// windows owned by the current user.
///
/// These are the exact strings that should be used in the `apps` field of
/// the keymapperd configuration.
#[cfg(target_os = "linux")]
pub fn list_app_names() -> Vec<String> {
    linux::list_app_names()
}

#[cfg(target_os = "macos")]
pub fn list_app_names() -> Vec<String> {
    macos::list_app_names()
}

#[cfg(target_os = "windows")]
pub fn list_app_names() -> Vec<String> {
    windows::list_app_names()
}
