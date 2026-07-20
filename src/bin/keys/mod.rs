// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Key introspection commands.  `list` prints all recognised key names;
//! `probe` waits for physical key presses and reports their canonical names.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use keymapperd::os::Key;

/// Print all key names recognised in the configuration file, sorted
/// alphabetically.
pub fn list() {
    let mut names: Vec<&str> = Key::ALL.iter().map(|k| k.as_str()).collect();
    names.sort();

    for name in names {
        println!("{name}");
    }
}

/// Wait for key presses and print the canonical name and native code for
/// each pressed key.  Exits when Control+Escape is pressed.
#[cfg(target_os = "macos")]
pub fn probe() {
    macos::probe()
}

#[cfg(target_os = "linux")]
pub fn probe() {
    linux::probe()
}

#[cfg(target_os = "windows")]
pub fn probe() {
    windows::probe()
}
