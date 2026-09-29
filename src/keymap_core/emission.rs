// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The single definition of what the platform backends do with a
//! [`Decision`].
//!
//! [`Decision`] interpretation used to be copy-pasted into each backend's
//! `match` (Linux `mapping/device.rs`, macOS `mapping.rs`, Windows
//! `mapping.rs`): release the clean-tap mask, then emit each output as a tap
//! or a hold via `output_held_mask`, release on swallow, and swallow-or-pass
//! a consumed modifier's release. A semantic change (a new variant, a
//! different hold rule) required three coordinated edits, and the verbatim
//! comments made partial updates easy to miss.
//!
//! [`emission_plan`] folds that shared semantics into one pure function that
//! turns a [`Decision`] into an ordered [`EmissionPlan`]. Each backend keeps
//! only its native primitives (forward / release / tap / hold) and its
//! platform-specific extras (macOS echo tracking and its additive
//! batch delivery, the Windows deferred re-emission and elevated-foreground
//! guard, Linux's out-of-lock deferred queue). The one place a `Decision`'s
//! meaning is defined, and one place the `emit` debug line is written, is
//! here.

use crate::keymap_core::{
    engine::{Decision, output_held_mask},
    logfmt,
    mapping_cache::NativeKey,
};

/// A platform-agnostic emission action, an element of an [`EmissionPlan`].
///
/// A backend maps each action onto its native primitive (Linux `emit_actions`,
/// the Windows `SendInput` helpers, the macOS `emit_native_key` batch). macOS
/// does not distinguish [`EmitAction::Tap`] from [`EmitAction::Hold`] — its
/// virtual keyboard taps every output — so it delivers both the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmitAction {
    /// Release these modifier bits on the output device (the fired trigger's
    /// consumed bits for a clean tap, or the held output bits being let go).
    /// Never `0`.
    ReleaseConsumed(u8),
    /// Emit a mapped output as a self-contained tap (modifiers, base,
    /// releases).
    Tap(NativeKey),
    /// Hold a mapped modifier-key output on the output device until the
    /// physical key-up, so a remapped modifier stays active for the key
    /// presses that follow it.
    Hold(NativeKey),
}

/// What the backend must do with the *input* event itself, independent of the
/// outputs it produced.
///
/// It drives both the `recv`/`pass`/`swal` log line and the backend's return
/// value (whether the input reaches the application or is swallowed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFate {
    /// Forward the input event to the application (an unmapped key, or a
    /// consumed modifier's release on a platform whose output device never saw
    /// the forwarded press). Log as `pass`; the hook/tap lets the event
    /// through.
    Forward,
    /// Swallow the input event without forwarding it. Log as `swal`.
    Swallow,
    /// Swallow the input event; it produced mapped outputs (delivered as
    /// [`EmitAction`]s), so there is no `pass`/`swal` line to log — the `emit`
    /// lines stand for it.
    Silent,
}

/// How a platform's output device treats a [`Decision::ConsumedRelease`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumedReleaseFate {
    /// The output device already released the modifier when the trigger fired
    /// (Linux, Windows): swallow the physical release.
    Swallow,
    /// Forwarded events never reach the output device (macOS): the physical
    /// release is the application's only source of truth, so it must pass
    /// through.
    //
    // Constructed only by the macOS backend, so it is never built on the
    // other hosts where this `pub(crate)` enum compiles.
    #[allow(dead_code)]
    PassThrough,
}

/// The ordered [`EmitAction`]s that carry out a [`Decision`], plus the fate of
/// the input event ([`InputFate`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmissionPlan {
    /// What to do with the input event itself.
    pub input: InputFate,
    /// The outputs to emit, in order. Empty when the decision emits nothing.
    pub actions: Vec<EmitAction>,
}

/// Translate a [`Decision`] into the [`EmissionPlan`] that carries it out.
///
/// This is the single home of `Decision` semantics:
///
/// - [`Decision::Pass`] → forward the input, emit nothing.
/// - [`Decision::Emit`] → release the clean-tap mask first (when non-zero),
///   then per output, [`Hold`] it if its base is a modifier key
///   (`output_held_mask` is `Some`) or [`Tap`] it otherwise. The input is
///   swallowed silently (the `emit` lines stand for it).
/// - [`Decision::Swallow`] → release the held-output bits (when non-zero);
///   swallow the input.
/// - [`Decision::ConsumedRelease`] → swallow or pass, per `consumed_release`.
///
/// Each mapped output's `emit` debug line is logged here, so the grammar has
/// one producing implementation (see [`logfmt`]).
///
/// [`Hold`]: EmitAction::Hold
/// [`Tap`]: EmitAction::Tap
pub fn emission_plan(
    decision: &Decision,
    consumed_release: ConsumedReleaseFate,
) -> EmissionPlan {
    match decision {
        Decision::Pass => EmissionPlan {
            input: InputFate::Forward,
            actions: Vec::new(),
        },
        Decision::Emit { release, outputs } => {
            let mut actions = Vec::with_capacity(outputs.len() + 1);
            if *release != 0 {
                actions.push(EmitAction::ReleaseConsumed(*release));
            }
            for output in outputs {
                logfmt::log_emit(output);
                actions.push(if output_held_mask(output).is_some() {
                    EmitAction::Hold(output.clone())
                } else {
                    EmitAction::Tap(output.clone())
                });
            }
            EmissionPlan {
                input: InputFate::Silent,
                actions,
            }
        }
        Decision::Swallow { release } => EmissionPlan {
            input: InputFate::Swallow,
            actions: if *release != 0 {
                vec![EmitAction::ReleaseConsumed(*release)]
            } else {
                Vec::new()
            },
        },
        Decision::ConsumedRelease => EmissionPlan {
            input: match consumed_release {
                ConsumedReleaseFate::Swallow => InputFate::Swallow,
                ConsumedReleaseFate::PassThrough => InputFate::Forward,
            },
            actions: Vec::new(),
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::hid_usage::HidUsage;

    fn nk(usage: HidUsage, modifiers: u8) -> NativeKey {
        NativeKey { modifiers, usage }
    }

    #[test]
    fn pass_forwards_input_and_emits_nothing() {
        let plan =
            emission_plan(&Decision::Pass, ConsumedReleaseFate::Swallow);
        assert_eq!(plan.input, InputFate::Forward);
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn emit_releases_clean_tap_mask_before_outputs() {
        // A chord trigger's consumed modifier bits are released before the
        // mapped output so the output is a clean tap.
        let plan = emission_plan(
            &Decision::Emit {
                release: 0b0000_0011,
                outputs: vec![nk(HidUsage::A, 0)],
            },
            ConsumedReleaseFate::Swallow,
        );
        assert_eq!(plan.input, InputFate::Silent);
        assert_eq!(
            plan.actions,
            vec![
                EmitAction::ReleaseConsumed(0b0000_0011),
                EmitAction::Tap(nk(HidUsage::A, 0)),
            ]
        );
    }

    #[test]
    fn emit_omits_release_when_mask_is_zero() {
        let plan = emission_plan(
            &Decision::Emit {
                release: 0,
                outputs: vec![nk(HidUsage::A, 0)],
            },
            ConsumedReleaseFate::Swallow,
        );
        assert_eq!(plan.actions, vec![EmitAction::Tap(nk(HidUsage::A, 0))]);
    }

    #[test]
    fn emit_holds_modifier_output_and_taps_regular_output() {
        // An output whose base is itself a modifier key is held; a regular key
        // is tapped. This is the shared hold-vs-tap rule the backends used to
        // duplicate.
        let plan = emission_plan(
            &Decision::Emit {
                release: 0,
                outputs: vec![nk(HidUsage::LeftShift, 0), nk(HidUsage::A, 0)],
            },
            ConsumedReleaseFate::Swallow,
        );
        assert_eq!(
            plan.actions,
            vec![
                EmitAction::Hold(nk(HidUsage::LeftShift, 0)),
                EmitAction::Tap(nk(HidUsage::A, 0)),
            ]
        );
    }

    #[test]
    fn swallow_releases_held_bits_then_swallows() {
        let plan = emission_plan(
            &Decision::Swallow {
                release: 0b0010_0000,
            },
            ConsumedReleaseFate::Swallow,
        );
        assert_eq!(plan.input, InputFate::Swallow);
        assert_eq!(
            plan.actions,
            vec![EmitAction::ReleaseConsumed(0b0010_0000)]
        );
    }

    #[test]
    fn swallow_without_release_has_no_actions() {
        let plan = emission_plan(
            &Decision::Swallow { release: 0 },
            ConsumedReleaseFate::Swallow,
        );
        assert_eq!(plan.input, InputFate::Swallow);
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn consumed_release_fate_decides_input_fate() {
        assert_eq!(
            emission_plan(
                &Decision::ConsumedRelease,
                ConsumedReleaseFate::Swallow,
            )
            .input,
            InputFate::Swallow
        );
        assert_eq!(
            emission_plan(
                &Decision::ConsumedRelease,
                ConsumedReleaseFate::PassThrough,
            )
            .input,
            InputFate::Forward
        );
    }
}
