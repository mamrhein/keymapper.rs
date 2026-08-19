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

use crate::common::hid_usage::HidUsage;

/// Print all recognised key names grouped by category, with abbreviated ranges
/// for large groups (letters, numbers, function keys, numpad digits).
pub fn list() {
    print_group(
        "Modifiers",
        &[
            HidUsage::LeftControl,
            HidUsage::RightControl,
            HidUsage::LeftShift,
            HidUsage::RightShift,
            HidUsage::LeftAlt,
            HidUsage::RightAlt,
            HidUsage::LeftCommand,
            HidUsage::RightCommand,
            HidUsage::CapsLock,
        ],
    );

    print_group(
        "Editor/misc",
        &[
            HidUsage::Tab,
            HidUsage::Space,
            HidUsage::Return,
            HidUsage::Backspace,
            HidUsage::Delete,
            HidUsage::Escape,
        ],
    );

    print_group(
        "Navigation",
        &[
            HidUsage::UpArrow,
            HidUsage::DownArrow,
            HidUsage::LeftArrow,
            HidUsage::RightArrow,
            HidUsage::PageUp,
            HidUsage::PageDown,
            HidUsage::Home,
            HidUsage::End,
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
            HidUsage::NumpadDecimal,
            HidUsage::NumpadMultiply,
            HidUsage::NumpadPlus,
            HidUsage::NumpadDivide,
            HidUsage::NumpadEnter,
            HidUsage::NumpadMinus,
        ],
    );

    // Numpad digit keys are printed as an abbreviated range.
    println!("    Numpad0 .. Numpad9");

    // Platform-specific numpad keys.
    print_numpad_platform_keys();

    print_group(
        "Symbols",
        &[
            HidUsage::Minus,
            HidUsage::Equal,
            HidUsage::BracketLeft,
            HidUsage::BracketRight,
            HidUsage::Backslash,
            HidUsage::Semicolon,
            HidUsage::Quote,
            HidUsage::Comma,
            HidUsage::Period,
            HidUsage::Slash,
            HidUsage::Grave,
            HidUsage::IsoExtra,
        ],
    );

    // Platform-specific symbol keys.
    print_symbols_platform_keys();

    // Consumer page media controls.
    println!("  Media:");
    println!("    {}", HidUsage::PlayPause.as_str());
    println!("    {}", HidUsage::VolumeUp.as_str());
    println!("    {}", HidUsage::VolumeDown.as_str());
    println!("    {}", HidUsage::Mute.as_str());
    println!("    {}", HidUsage::NextTrack.as_str());
    println!("    {}", HidUsage::PreviousTrack.as_str());
    println!("    {}", HidUsage::Stop.as_str());

    // Consumer page display controls.
    println!("  Display:");
    println!("    {}", HidUsage::BrightnessUp.as_str());
    println!("    {}", HidUsage::BrightnessDown.as_str());

    println!();
    println!("Total: {} keys", HidUsage::ALL.len());
}

#[cfg(target_os = "macos")]
fn print_numpad_platform_keys() {
    // All numpad keys are available via HidUsage.
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn print_numpad_platform_keys() {
    // Linux and Windows have no platform-specific numpad keys.
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn print_symbols_platform_keys() {
    println!("    {}", HidUsage::IsoHash.as_str());
}

#[cfg(target_os = "macos")]
fn print_symbols_platform_keys() {
    // macOS has no platform-specific symbol keys.
}

/// Print a group header and its keys.
fn print_group(name: &str, keys: &[HidUsage]) {
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
