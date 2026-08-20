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

use crate::common::hid_usage::{HidUsage, PAGE_CONSUMER};

/// Category a `HidUsage` is listed under by `keys list`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ListCategory {
    /// Modifier keys, including CapsLock.
    Modifiers,
    /// Tab, space, enter, and other editing keys.
    Editor,
    /// Arrow keys and other navigation keys.
    Navigation,
    /// Non-digit numpad keys (the digits are printed as a range).
    Numpad,
    /// Punctuation and symbol keys, including the ISO extra keys.
    Symbols,
    /// Consumer page media controls.
    Media,
    /// Consumer page display controls.
    Display,
    /// Keys that are printed as an abbreviated range instead of
    /// individually (function keys, letters, number row, numpad digits).
    Range,
}

/// Assign a usage to its `keys list` category.
///
/// The match is exhaustive on purpose: the compiler rejects any new
/// `HidUsage` variant until it has been assigned to a category here, so
/// the listing cannot silently lose keys.
fn list_category(usage: HidUsage) -> ListCategory {
    match usage {
        // Modifiers, including CapsLock.
        HidUsage::LeftControl
        | HidUsage::RightControl
        | HidUsage::LeftShift
        | HidUsage::RightShift
        | HidUsage::LeftAlt
        | HidUsage::RightAlt
        | HidUsage::LeftCommand
        | HidUsage::RightCommand
        | HidUsage::CapsLock => ListCategory::Modifiers,

        // Editor and misc.
        HidUsage::Tab
        | HidUsage::Space
        | HidUsage::Return
        | HidUsage::Backspace
        | HidUsage::Delete
        | HidUsage::Escape => ListCategory::Editor,

        // Navigation.
        HidUsage::UpArrow
        | HidUsage::DownArrow
        | HidUsage::LeftArrow
        | HidUsage::RightArrow
        | HidUsage::PageUp
        | HidUsage::PageDown
        | HidUsage::Home
        | HidUsage::End => ListCategory::Navigation,

        // Function keys, letters, number row, and numpad digits are
        // printed as abbreviated ranges.
        HidUsage::F1
        | HidUsage::F2
        | HidUsage::F3
        | HidUsage::F4
        | HidUsage::F5
        | HidUsage::F6
        | HidUsage::F7
        | HidUsage::F8
        | HidUsage::F9
        | HidUsage::F10
        | HidUsage::F11
        | HidUsage::F12
        | HidUsage::A
        | HidUsage::B
        | HidUsage::C
        | HidUsage::D
        | HidUsage::E
        | HidUsage::F
        | HidUsage::G
        | HidUsage::H
        | HidUsage::I
        | HidUsage::J
        | HidUsage::K
        | HidUsage::L
        | HidUsage::M
        | HidUsage::N
        | HidUsage::O
        | HidUsage::P
        | HidUsage::Q
        | HidUsage::R
        | HidUsage::S
        | HidUsage::T
        | HidUsage::U
        | HidUsage::V
        | HidUsage::W
        | HidUsage::X
        | HidUsage::Y
        | HidUsage::Z
        | HidUsage::Number0
        | HidUsage::Number1
        | HidUsage::Number2
        | HidUsage::Number3
        | HidUsage::Number4
        | HidUsage::Number5
        | HidUsage::Number6
        | HidUsage::Number7
        | HidUsage::Number8
        | HidUsage::Number9
        | HidUsage::Numpad0
        | HidUsage::Numpad1
        | HidUsage::Numpad2
        | HidUsage::Numpad3
        | HidUsage::Numpad4
        | HidUsage::Numpad5
        | HidUsage::Numpad6
        | HidUsage::Numpad7
        | HidUsage::Numpad8
        | HidUsage::Numpad9 => ListCategory::Range,

        // Numpad (non-digit).
        HidUsage::NumpadDecimal
        | HidUsage::NumpadMultiply
        | HidUsage::NumpadPlus
        | HidUsage::NumpadDivide
        | HidUsage::NumpadEnter
        | HidUsage::NumpadMinus
        | HidUsage::NumpadClear
        | HidUsage::NumpadEqual => ListCategory::Numpad,

        // Symbols, including the ISO extra keys.
        HidUsage::Minus
        | HidUsage::Equal
        | HidUsage::BracketLeft
        | HidUsage::BracketRight
        | HidUsage::Backslash
        | HidUsage::Semicolon
        | HidUsage::Quote
        | HidUsage::Grave
        | HidUsage::Comma
        | HidUsage::Slash
        | HidUsage::Period
        | HidUsage::IsoExtra
        | HidUsage::IsoHash => ListCategory::Symbols,

        // Consumer page media controls.
        HidUsage::PlayPause
        | HidUsage::VolumeUp
        | HidUsage::VolumeDown
        | HidUsage::Mute
        | HidUsage::NextTrack
        | HidUsage::PreviousTrack
        | HidUsage::Stop => ListCategory::Media,

        // Consumer page display controls.
        HidUsage::BrightnessUp | HidUsage::BrightnessDown => {
            ListCategory::Display
        }
    }
}

/// Print all recognised key names grouped by category, with abbreviated
/// ranges for large groups (letters, numbers, function keys, numpad
/// digits).
///
/// The listing is derived from `HidUsage::all()`: every defined usage is
/// printed exactly once, and Consumer Page usages carry their usage id.
pub fn list() {
    // Bucket all usages in a single pass; the order inside a bucket
    // follows `HidUsage::ALL`.
    let mut modifiers = Vec::new();
    let mut editor = Vec::new();
    let mut navigation = Vec::new();
    let mut numpad = Vec::new();
    let mut symbols = Vec::new();
    let mut media = Vec::new();
    let mut display = Vec::new();

    for &usage in HidUsage::all() {
        match list_category(usage) {
            ListCategory::Modifiers => modifiers.push(usage),
            ListCategory::Editor => editor.push(usage),
            ListCategory::Navigation => navigation.push(usage),
            ListCategory::Numpad => numpad.push(usage),
            ListCategory::Symbols => symbols.push(usage),
            ListCategory::Media => media.push(usage),
            ListCategory::Display => display.push(usage),
            // Printed as abbreviated ranges below.
            ListCategory::Range => {}
        }
    }

    print_group("Modifiers", &modifiers);
    print_group("Editor/misc", &editor);
    print_group("Navigation", &navigation);

    // These groups are printed as abbreviated ranges.
    println!("  Function keys:");
    println!("    F1 .. F12");
    println!("  Letters:");
    println!("    A .. Z");
    println!("  Numbers:");
    println!("    0 .. 9");

    print_group("Numpad", &numpad);
    println!("    Numpad0 .. Numpad9");
    print_group("Symbols", &symbols);
    print_group("Media", &media);
    print_group("Display", &display);

    println!();
    println!("Total: {} keys", HidUsage::ALL.len());
}

/// Print a group header and its keys.
///
/// Consumer Page usages are annotated with their usage id (e.g.
/// `PlayPause  (consumer 0xCD)`), because the name alone does not encode
/// the page.
fn print_group(name: &str, keys: &[HidUsage]) {
    println!("  {name}:");
    for k in keys {
        if k.page() == PAGE_CONSUMER {
            println!("    {}  (consumer 0x{:02X})", k.as_str(), k.id());
        } else {
            println!("    {}", k.as_str());
        }
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
