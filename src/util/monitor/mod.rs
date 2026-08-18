// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Cross-platform keyboard event monitor for e2e testing.
//!
//! Creates a small egui window that captures keyboard events and logs them
//! to an output file. Used by the e2e test harness to record actual key
//! events received by a focused window.

use std::sync::{Arc, atomic::AtomicBool};

use egui::Key as EguiKey;

use crate::common::Key;

pub mod app;
#[cfg(target_os = "linux")]
pub(crate) mod linux;
pub mod writer;

/// A single captured keyboard event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputEvent {
    /// Whether the key was pressed (true) or released (false).
    pub down: bool,
    /// The logical key that changed state.
    pub key: Key,
}

/// State shared between the egui application loop and the signal handler.
pub struct MonitorState {
    /// Set to `true` by the signal handler on SIGTERM/SIGINT.
    pub shutdown: Arc<AtomicBool>,
}

/// Tracks the pressed state of all modifier keys.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModifierState {
    pub left_control: bool,
    pub left_shift: bool,
    pub left_alt: bool,
    pub left_super: bool,
}

/// Register unix signal handlers for graceful shutdown.
///
/// Returns an `AtomicBool` that is set to `true` when a shutdown signal
/// (SIGINT or SIGTERM) is received.
#[cfg(unix)]
pub fn register_signal_handlers() -> Arc<AtomicBool> {
    use signal_hook::{
        consts::signal::{SIGINT, SIGTERM},
        flag,
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    flag::register(SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    flag::register(SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");
    shutdown
}

/// No-op signal registration on Windows.
#[cfg(not(unix))]
pub fn register_signal_handlers() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// Map an egui `Key` to our platform-agnostic `Key`.
///
/// Returns `None` for keys we don't track. Modifier keys are NOT in egui's
/// `keys_down` set - they are tracked via the `Modifiers` struct instead.
pub fn map_egui_key(egui_key: EguiKey) -> Option<Key> {
    match egui_key {
        // Navigation
        EguiKey::ArrowDown => Some(Key::DownArrow),
        EguiKey::ArrowLeft => Some(Key::LeftArrow),
        EguiKey::ArrowRight => Some(Key::RightArrow),
        EguiKey::ArrowUp => Some(Key::UpArrow),
        // Editor / misc
        EguiKey::Tab => Some(Key::Tab),
        EguiKey::Space => Some(Key::Space),
        EguiKey::Enter => Some(Key::Return),
        EguiKey::Backspace => Some(Key::Backspace),
        EguiKey::Delete => Some(Key::Delete),
        EguiKey::Escape => Some(Key::Escape),
        EguiKey::Home => Some(Key::Home),
        EguiKey::End => Some(Key::End),
        EguiKey::PageUp => Some(Key::PageUp),
        EguiKey::PageDown => Some(Key::PageDown),
        // Function keys
        EguiKey::F1 => Some(Key::F1),
        EguiKey::F2 => Some(Key::F2),
        EguiKey::F3 => Some(Key::F3),
        EguiKey::F4 => Some(Key::F4),
        EguiKey::F5 => Some(Key::F5),
        EguiKey::F6 => Some(Key::F6),
        EguiKey::F7 => Some(Key::F7),
        EguiKey::F8 => Some(Key::F8),
        EguiKey::F9 => Some(Key::F9),
        EguiKey::F10 => Some(Key::F10),
        EguiKey::F11 => Some(Key::F11),
        EguiKey::F12 => Some(Key::F12),
        // Letters
        EguiKey::A => Some(Key::A),
        EguiKey::B => Some(Key::B),
        EguiKey::C => Some(Key::C),
        EguiKey::D => Some(Key::D),
        EguiKey::E => Some(Key::E),
        EguiKey::F => Some(Key::F),
        EguiKey::G => Some(Key::G),
        EguiKey::H => Some(Key::H),
        EguiKey::I => Some(Key::I),
        EguiKey::J => Some(Key::J),
        EguiKey::K => Some(Key::K),
        EguiKey::L => Some(Key::L),
        EguiKey::M => Some(Key::M),
        EguiKey::N => Some(Key::N),
        EguiKey::O => Some(Key::O),
        EguiKey::P => Some(Key::P),
        EguiKey::Q => Some(Key::Q),
        EguiKey::R => Some(Key::R),
        EguiKey::S => Some(Key::S),
        EguiKey::T => Some(Key::T),
        EguiKey::U => Some(Key::U),
        EguiKey::V => Some(Key::V),
        EguiKey::W => Some(Key::W),
        EguiKey::X => Some(Key::X),
        EguiKey::Y => Some(Key::Y),
        EguiKey::Z => Some(Key::Z),
        // Numbers (top row)
        EguiKey::Num1 => Some(Key::Number1),
        EguiKey::Num2 => Some(Key::Number2),
        EguiKey::Num3 => Some(Key::Number3),
        EguiKey::Num4 => Some(Key::Number4),
        EguiKey::Num5 => Some(Key::Number5),
        EguiKey::Num6 => Some(Key::Number6),
        EguiKey::Num7 => Some(Key::Number7),
        EguiKey::Num8 => Some(Key::Number8),
        EguiKey::Num9 => Some(Key::Number9),
        EguiKey::Num0 => Some(Key::Number0),
        // Punctuation / symbols
        EguiKey::Minus => Some(Key::Minus),
        EguiKey::Equals => Some(Key::Equal),
        EguiKey::OpenBracket => Some(Key::BracketLeft),
        EguiKey::CloseBracket => Some(Key::BracketRight),
        EguiKey::Backslash => Some(Key::Backslash),
        EguiKey::Semicolon => Some(Key::Semicolon),
        EguiKey::Quote => Some(Key::Quote),
        EguiKey::Comma => Some(Key::Comma),
        EguiKey::Period => Some(Key::Period),
        EguiKey::Slash => Some(Key::Slash),
        EguiKey::Backtick => Some(Key::Grave),
        // Keys we don't track (Copy, Cut, Paste, Insert, etc.).
        _ => None,
    }
}
