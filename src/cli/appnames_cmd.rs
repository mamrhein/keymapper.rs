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
pub fn run() {
    let apps = app_identity::list_app_names();

    if apps.is_empty() {
        println!("No visible applications found.");
        return;
    }

    // Align the display names in a second column, but only print that
    // column for entries where it adds information.
    let width = apps.iter().map(|app| app.name.len()).max().unwrap_or(0);
    for app in &apps {
        if app.display == app.name {
            println!("{}", app.name);
        } else {
            println!("{:<width$}  {}", app.name, app.display);
        }
    }
}
