// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The unified debug-log grammar shared by all three platform backends.
//!
//! The e2e harness verifies a phase by reading the daemon's own debug log and
//! parsing the `recv`/`pass`/`swal`/`emit` lines (see `tests/log_verify.rs`).
//! That makes the debug-log text a de-facto API: a reworded line in any one
//! backend silently breaks verification on that platform.  This module is the
//! single producing implementation of that grammar, so the contract lives in
//! one place a contributor can read and test against, and `log_verify`'s
//! parser is pinned to it by a round-trip unit test.
//!
//! The grammar:
//!
//! ```text
//! recv <native> <down|up|repeat> -> <usage>
//! pass <native> <down|up|repeat> -> <usage>
//! swal <native> <down|up|repeat> -> <usage>
//! emit <native_key>
//! ```
//!
//! `<native>` is platform-specific free text (a device path and key code on
//! Linux, a `keycode=` on macOS, a `vk=` on Windows).  Callers pass it as a
//! [`std::fmt::Arguments`] fragment: it borrows the caller's locals and is
//! only rendered when the line's level is actually enabled, so the
//! always-executed `recv`/`pass` lines stay allocation-free at the production
//! `info` level.
//!
//! The level selection is part of the contract and is owned here: the down and
//! the auto-repeat are the informative events and are logged at `debug`; the
//! key-up is kept at `trace` so it stays out of the default debug output; a
//! swallow is never the informative event, so it is always `trace`.

use std::fmt::Arguments;

use log::{Level, debug, trace};

use crate::{
    common::{config::KeyEvent, hid_usage::HidUsage, modifier::ModifierRole},
    keymap_core::mapping_cache::{NativeKey, compile_modifier_bits},
};

/// The direction of a key event in the `recv`/`pass`/`swal` line.
///
/// It selects the line's level (`Direction::level`) and renders as the single
/// single word between `<native>` and `-> <usage>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// A key-down.
    Down,
    /// A key-up.
    Up,
    /// An auto-repeat of a key that is still held (a down for the engine).
    Repeat,
}

impl Direction {
    /// The direction for a plain key-down / key-up pair.
    pub fn from_is_down(is_down: bool) -> Self {
        if is_down { Self::Down } else { Self::Up }
    }

    /// The single word rendered between `<native>` and `-> <usage>`.
    fn as_str(self) -> &'static str {
        match self {
            Direction::Down => "down",
            Direction::Up => "up",
            Direction::Repeat => "repeat",
        }
    }

    /// The level a `recv`/`pass` line of this direction is logged at: the up
    /// is `trace`, the down and the repeat are `debug`.
    fn level(self) -> Level {
        match self {
            Direction::Up => Level::Trace,
            Direction::Down | Direction::Repeat => Level::Debug,
        }
    }
}

// ---------------------------------------------------------------------------
// Emission (production): the `recv`/`pass`/`swal`/`emit`/`map` lines
// ---------------------------------------------------------------------------

/// Log that the daemon received a key: `recv <native> <dir> -> <usage>`.
pub fn log_recv(native: Arguments<'_>, dir: Direction, usage: HidUsage) {
    if dir.level() == Level::Trace {
        trace!("recv {native} {} -> {usage}", dir.as_str());
    } else {
        debug!("recv {native} {} -> {usage}", dir.as_str());
    }
}

/// Log that the daemon forwarded a key to the application:
/// `pass <native> <dir> -> <usage>`.
pub fn log_pass(native: Arguments<'_>, dir: Direction, usage: HidUsage) {
    if dir.level() == Level::Trace {
        trace!("pass {native} {} -> {usage}", dir.as_str());
    } else {
        debug!("pass {native} {} -> {usage}", dir.as_str());
    }
}

/// Log that the daemon swallowed a key: `swal <native> <dir> -> <usage>`.
///
/// A swallow is a mapped key-up or the repeat of a mapped key — never the
/// informative event — so it is always logged at `trace`.
pub fn log_swal(native: Arguments<'_>, dir: Direction, usage: HidUsage) {
    trace!("swal {native} {} -> {usage}", dir.as_str());
}

/// Log a mapped output: `emit <native_key>`.
pub fn log_emit(output: &NativeKey) {
    debug!("emit {}", fmt_native_key(output));
}

/// Log the engine's mapping decision: `map <mods+key> -> [outputs]`.
///
/// The `map` line is not part of the e2e-verified grammar (the parser ignores
/// it), but its wording lives here with the rest so the whole debug-log format
/// has one home.
pub fn log_map(lookup_modifiers: u8, usage: HidUsage, outputs: &[NativeKey]) {
    debug!(
        "map {} -> [{}]",
        fmt_modifiers_and_key(lookup_modifiers, usage),
        fmt_native_keys(outputs)
    );
}

// ---------------------------------------------------------------------------
// Formatting (the harness contract: exactly the strings the functions above
// emit, so the e2e parser can be round-trip tested against the producer)
// ---------------------------------------------------------------------------

/// Render a [`recv`] line body.
///
/// [`recv`]: log_recv
pub fn fmt_recv(
    native: Arguments<'_>,
    dir: Direction,
    usage: HidUsage,
) -> String {
    format!("recv {native} {} -> {usage}", dir.as_str())
}

/// Render a [`pass`] line body.
///
/// [`pass`]: log_pass
pub fn fmt_pass(
    native: Arguments<'_>,
    dir: Direction,
    usage: HidUsage,
) -> String {
    format!("pass {native} {} -> {usage}", dir.as_str())
}

/// Render a [`swal`] line body.
///
/// [`swal`]: log_swal
pub fn fmt_swal(
    native: Arguments<'_>,
    dir: Direction,
    usage: HidUsage,
) -> String {
    format!("swal {native} {} -> {usage}", dir.as_str())
}

/// Render an [`emit`] line body.
///
/// [`emit`]: log_emit
pub fn fmt_emit(output: &NativeKey) -> String {
    format!("emit {}", fmt_native_key(output))
}

// ---------------------------------------------------------------------------
// Key rendering
// ---------------------------------------------------------------------------

/// The config canonical names of the modifier bits set in *mask*, in bit
/// order. Empty when no modifier is held.
fn modifier_names(mask: u8) -> Vec<&'static str> {
    (0..8u8)
        .filter(|&bit| (mask >> bit) & 1 == 1)
        .filter_map(|bit| {
            let role = ModifierRole::try_from_bit(bit)?;
            HidUsage::keyboard(role.hid_id()).map(HidUsage::as_str)
        })
        .collect()
}

/// Render a modifier mask and a HID usage for debug logging: the held modifier
/// names and the key's canonical name joined with `+`
/// (e.g. `LeftControl+LeftShift+A`, or just `A` when no modifier is held).
pub(crate) fn fmt_modifiers_and_key(mask: u8, key: HidUsage) -> String {
    let mut parts = modifier_names(mask);
    parts.push(key.as_str());
    parts.join("+")
}

/// Render a [`NativeKey`] for debug logging: the held modifier names joined
/// with `+` to the base key's canonical name (e.g. `LeftControl+LeftShift+A`,
/// or just `A` when no modifier is held).
#[inline(always)]
pub(crate) fn fmt_native_key(key: &NativeKey) -> String {
    fmt_modifiers_and_key(key.modifiers, key.usage)
}

/// Render a slice of [`NativeKey`]s as a comma-separated list for debug
/// logging, reusing [`fmt_native_key`] so every site reads uniformly.
pub(crate) fn fmt_native_keys(keys: &[NativeKey]) -> String {
    keys.iter()
        .map(fmt_native_key)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render an output [`KeyEvent`] exactly as the daemon logs it in an `emit`
/// line: compile its held modifiers to a bitmask and format the resulting
/// [`NativeKey`] (e.g. `LeftCommand+A`, or just `A` when no modifier is held).
///
/// Public so the e2e harness can build its expected emit sequence from the
/// config and compare it, string for string, against the daemon's log.
pub fn fmt_key_event(event: &KeyEvent) -> String {
    fmt_native_key(&NativeKey {
        modifiers: compile_modifier_bits(&event.modifiers),
        usage: event.base,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_renders_as_expected_word() {
        assert_eq!(Direction::Down.as_str(), "down");
        assert_eq!(Direction::Up.as_str(), "up");
        assert_eq!(Direction::Repeat.as_str(), "repeat");
    }

    #[test]
    fn direction_from_is_down() {
        assert_eq!(Direction::from_is_down(true), Direction::Down);
        assert_eq!(Direction::from_is_down(false), Direction::Up);
    }

    #[test]
    fn up_is_trace_down_and_repeat_are_debug() {
        // The level selection is part of the verified contract: the key-up is
        // kept out of the default debug output while the down and its repeats
        // stay visible.
        assert_eq!(Direction::Up.level(), Level::Trace);
        assert_eq!(Direction::Down.level(), Level::Debug);
        assert_eq!(Direction::Repeat.level(), Level::Debug);
    }

    #[test]
    fn fmt_renders_modifiers_and_keys() {
        assert_eq!(
            fmt_native_key(&NativeKey {
                modifiers: 0,
                usage: HidUsage::A
            }),
            "A"
        );
        assert_eq!(
            fmt_native_key(&NativeKey {
                modifiers: 0b0000_0011,
                usage: HidUsage::A
            }),
            "LeftControl+LeftShift+A"
        );
    }

    #[test]
    fn fmt_native_keys_is_comma_separated() {
        assert_eq!(
            fmt_native_keys(&[
                NativeKey {
                    modifiers: 0,
                    usage: HidUsage::A
                },
                NativeKey {
                    modifiers: 0b0000_0001,
                    usage: HidUsage::B
                },
            ]),
            "A, LeftControl+B"
        );
    }

    #[test]
    fn fmt_key_event_compiles_modifier_bits() {
        let event = KeyEvent {
            base: HidUsage::A,
            modifiers: vec![HidUsage::LeftCommand],
        };
        assert_eq!(fmt_key_event(&event), "LeftCommand+A");
    }
}
