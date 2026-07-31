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

use crate::platform::Key;

/// Print all recognised key names grouped by category, with abbreviated ranges
/// for large groups (letters, numbers, function keys, numpad digits).
pub fn list() {
    print_group(
        "Modifiers",
        &[
            Key::LeftControl,
            Key::RightControl,
            Key::LeftShift,
            Key::RightShift,
            Key::LeftAlt,
            Key::RightAlt,
            Key::LeftCommand,
            Key::RightCommand,
            Key::CapsLock,
        ],
    );

    print_group(
        "Editor/misc",
        &[
            Key::Tab,
            Key::Space,
            Key::Return,
            Key::Backspace,
            Key::Delete,
            Key::Escape,
        ],
    );

    print_group(
        "Navigation",
        &[
            Key::UpArrow,
            Key::DownArrow,
            Key::LeftArrow,
            Key::RightArrow,
            Key::PageUp,
            Key::PageDown,
            Key::Home,
            Key::End,
        ],
    );

    // Function keys are printed as an abbreviated range.
    println!("  Function keys:");
    println!("    F1 .. F12");

    // Letters are printed as an abbreviated range.
    println!("  Letters:");
    println!("    A .. Z");

    // Number keys are printed as an abbreviated range.
    println!("  Numbers:");
    println!("    0 .. 9");

    print_group(
        "Numpad",
        &[
            Key::NumpadDecimal,
            Key::NumpadMultiply,
            Key::NumpadPlus,
            Key::NumpadDivide,
            Key::NumpadEnter,
            Key::NumpadMinus,
        ],
    );

    // Numpad digit keys are printed as an abbreviated range.
    println!("    Numpad0 .. Numpad9");

    // Platform-specific numpad keys.
    print_numpad_platform_keys();

    print_group(
        "Symbols",
        &[
            Key::Minus,
            Key::Equal,
            Key::BracketLeft,
            Key::BracketRight,
            Key::Backslash,
            Key::Semicolon,
            Key::Quote,
            Key::Comma,
            Key::Period,
            Key::Slash,
            Key::Grave,
            Key::IsoExtra,
        ],
    );

    // Platform-specific symbol keys.
    print_symbols_platform_keys();

    println!();
    println!("Total: {} keys", Key::ALL.len());
}

#[cfg(target_os = "macos")]
fn print_numpad_platform_keys() {
    println!("    {}", Key::NumpadClear.as_str());
    println!("    {}", Key::NumpadEqual.as_str());
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn print_numpad_platform_keys() {
    // Linux and Windows have no platform-specific numpad keys.
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn print_symbols_platform_keys() {
    println!("    {}", Key::IsoHash.as_str());
}

#[cfg(target_os = "macos")]
fn print_symbols_platform_keys() {
    // macOS has no platform-specific symbol keys.
}

/// Print a group header and its keys.
fn print_group(name: &str, keys: &[Key]) {
    println!("  {name}:");
    for k in keys {
        println!("    {}", k.as_str());
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
