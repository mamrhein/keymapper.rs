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
//! - [`get_active_app_name`] returns the canonical name of the current
//!   foreground application, used by the daemon for rule matching.
//! - [`list_app_names`] returns the canonical names of all visible
//!   applications (with a human-readable display alias for each), printed by
//!   `keymapper appnames`.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// A single application entry as returned by [`list_app_names`].
///
/// `name` is the canonical application identifier — the value to use in the
/// `apps` field of the keymapperd configuration.  `display` is a
/// human-readable alias (for example the application's marketing name) that
/// may or may not differ from `name`.
#[derive(Debug, Clone)]
pub struct AppName {
    /// Canonical app name used for rule matching.
    pub name: String,
    /// Human-readable display alias.
    pub display: String,
}

/// Synchronously query the canonical name of the current foreground
/// application.
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

/// Return the sorted, deduplicated list of application entries for all
/// visible windows owned by the current user.
///
/// The `name` field of each entry is the exact value to use in the `apps`
/// field of the keymapperd configuration.
#[cfg(target_os = "linux")]
pub fn list_app_names() -> Vec<AppName> {
    linux::list_app_names()
        .into_iter()
        .map(|name| AppName {
            display: name.clone(),
            name,
        })
        .collect()
}

#[cfg(target_os = "macos")]
pub fn list_app_names() -> Vec<AppName> {
    macos::list_app_names()
        .into_iter()
        .map(|name| AppName {
            display: name.clone(),
            name,
        })
        .collect()
}

#[cfg(target_os = "windows")]
pub fn list_app_names() -> Vec<AppName> {
    windows::list_app_names()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon and the CLI must agree on the app name namespace: the
    /// value returned by `get_active_app_name` must be one of the names
    /// `list_app_names` prints, so a rule scoped to an `appnames` value can
    /// actually match the active app.
    ///
    /// In headless environments the active query returns `"unknown"` and
    /// the test passes trivially.
    #[test]
    fn active_app_name_is_in_app_name_list() {
        let active = get_active_app_name();
        if active == "unknown" || active.is_empty() {
            return;
        }

        let names = list_app_names();
        assert!(
            names.iter().any(|entry| entry.name == active),
            "active app {active:?} is not among the visible app names: \
             {names:?}",
        );
    }
}
x
x
