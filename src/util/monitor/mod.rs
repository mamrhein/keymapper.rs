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

use crate::common::hid_usage::HidUsage;

pub mod app;
#[cfg(target_os = "linux")]
pub(crate) mod linux;
pub mod writer;

/// A single captured keyboard event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputEvent {
    /// Whether the key was pressed (true) or released (false).
    pub down: bool,
    /// The key that changed state.
    pub key: HidUsage,
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

/// Map an egui `Key` to its `HidUsage`.
///
/// Returns `None` for keys we don't track. Modifier keys are NOT in egui's
/// `keys_down` set - they are tracked via the `Modifiers` struct instead.
pub fn map_egui_key(egui_key: EguiKey) -> Option<HidUsage> {
    match egui_key {
        // Navigation
        EguiKey::ArrowDown => Some(HidUsage::DownArrow),
        EguiKey::ArrowLeft => Some(HidUsage::LeftArrow),
        EguiKey::ArrowRight => Some(HidUsage::RightArrow),
        EguiKey::ArrowUp => Some(HidUsage::UpArrow),
        // Editor / misc
        EguiKey::Tab => Some(HidUsage::Tab),
        EguiKey::Space => Some(HidUsage::Space),
        EguiKey::Enter => Some(HidUsage::Return),
        EguiKey::Backspace => Some(HidUsage::Backspace),
        EguiKey::Delete => Some(HidUsage::Delete),
        EguiKey::Escape => Some(HidUsage::Escape),
        EguiKey::Home => Some(HidUsage::Home),
        EguiKey::End => Some(HidUsage::End),
        EguiKey::PageUp => Some(HidUsage::PageUp),
        EguiKey::PageDown => Some(HidUsage::PageDown),
        // Function keys
        EguiKey::F1 => Some(HidUsage::F1),
        EguiKey::F2 => Some(HidUsage::F2),
        EguiKey::F3 => Some(HidUsage::F3),
        EguiKey::F4 => Some(HidUsage::F4),
        EguiKey::F5 => Some(HidUsage::F5),
        EguiKey::F6 => Some(HidUsage::F6),
        EguiKey::F7 => Some(HidUsage::F7),
        EguiKey::F8 => Some(HidUsage::F8),
        EguiKey::F9 => Some(HidUsage::F9),
        EguiKey::F10 => Some(HidUsage::F10),
        EguiKey::F11 => Some(HidUsage::F11),
        EguiKey::F12 => Some(HidUsage::F12),
        // Letters
        EguiKey::A => Some(HidUsage::A),
        EguiKey::B => Some(HidUsage::B),
        EguiKey::C => Some(HidUsage::C),
        EguiKey::D => Some(HidUsage::D),
        EguiKey::E => Some(HidUsage::E),
        EguiKey::F => Some(HidUsage::F),
        EguiKey::G => Some(HidUsage::G),
        EguiKey::H => Some(HidUsage::H),
        EguiKey::I => Some(HidUsage::I),
        EguiKey::J => Some(HidUsage::J),
        EguiKey::K => Some(HidUsage::K),
        EguiKey::L => Some(HidUsage::L),
        EguiKey::M => Some(HidUsage::M),
        EguiKey::N => Some(HidUsage::N),
        EguiKey::O => Some(HidUsage::O),
        EguiKey::P => Some(HidUsage::P),
        EguiKey::Q => Some(HidUsage::Q),
        EguiKey::R => Some(HidUsage::R),
        EguiKey::S => Some(HidUsage::S),
        EguiKey::T => Some(HidUsage::T),
        EguiKey::U => Some(HidUsage::U),
        EguiKey::V => Some(HidUsage::V),
        EguiKey::W => Some(HidUsage::W),
        EguiKey::X => Some(HidUsage::X),
        EguiKey::Y => Some(HidUsage::Y),
        EguiKey::Z => Some(HidUsage::Z),
        // Numbers (top row)
        EguiKey::Num1 => Some(HidUsage::Number1),
        EguiKey::Num2 => Some(HidUsage::Number2),
        EguiKey::Num3 => Some(HidUsage::Number3),
        EguiKey::Num4 => Some(HidUsage::Number4),
        EguiKey::Num5 => Some(HidUsage::Number5),
        EguiKey::Num6 => Some(HidUsage::Number6),
        EguiKey::Num7 => Some(HidUsage::Number7),
        EguiKey::Num8 => Some(HidUsage::Number8),
        EguiKey::Num9 => Some(HidUsage::Number9),
        EguiKey::Num0 => Some(HidUsage::Number0),
        // Punctuation / symbols
        EguiKey::Minus => Some(HidUsage::Minus),
        EguiKey::Equals => Some(HidUsage::Equal),
        EguiKey::OpenBracket => Some(HidUsage::BracketLeft),
        EguiKey::CloseBracket => Some(HidUsage::BracketRight),
        EguiKey::Backslash => Some(HidUsage::Backslash),
        EguiKey::Semicolon => Some(HidUsage::Semicolon),
        EguiKey::Quote => Some(HidUsage::Quote),
        EguiKey::Comma => Some(HidUsage::Comma),
        EguiKey::Period => Some(HidUsage::Period),
        EguiKey::Slash => Some(HidUsage::Slash),
        EguiKey::Backtick => Some(HidUsage::Grave),
        // Keys we don't track (Copy, Cut, Paste, Insert, etc.).
        _ => None,
    }
}
