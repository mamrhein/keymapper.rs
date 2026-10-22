// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! `keymapper appnames` — list applications that own visible windows.

use crate::platform::app_identity;

/// Print the canonical app name — the exact value to use in the `apps` field
/// of a config — followed by a human-readable display name where it differs.
///
/// The application currently in the foreground is marked with a leading `*`
/// in a dedicated marker column, so the name column stays copy-pasteable.
pub fn run() {
    let apps = app_identity::list_app_names();

    if apps.is_empty() {
        println!("No visible applications found.");
        return;
    }

    // The foreground app is resolved by a different code path than the list,
    // so match it case-insensitively — the same rule the daemon uses when
    // matching `apps`. A null result means none was found.
    let active = app_identity::get_active_app_name();
    let active = match active.as_str() {
        "" | "unknown" => None,
        name => Some(name),
    };

    // Align the display names in a second column, but only print that
    // column for entries where it adds information. The leading marker
    // column is always present so the name column stays aligned.
    let width = apps.iter().map(|app| app.name.len()).max().unwrap_or(0);
    for app in &apps {
        let marker = if active
            .is_some_and(|name| app.name.eq_ignore_ascii_case(name))
        {
            "*"
        } else {
            " "
        };
        if app.display == app.name {
            println!("{marker} {}", app.name);
        } else {
            println!("{marker} {:<width$}  {}", app.name, app.display);
        }
    }
}
