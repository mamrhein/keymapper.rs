// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Platform-agnostic key-mapping decision core.
//!
//! This module is the shared, testable heart of the macOS capture path.  It
//! decides, for every key event, whether the event should be passed through
//! unchanged, swallowed and re-emitted as a mapped output, or swallowed
//! without emitting.  It is deliberately free of platform dependencies: it
//! only depends on the shared [`HidUsage`], [`NativeKey`], and [`Lookup`]
//! types, so it can be unit-tested in isolation with the [`TestLookup`]
//! harness.
//!
//! The decision core is a simplified mirror of the legacy IOKit seizure
//! capture logic, minus the virtual-keyboard forwarding machinery.  Unmapped
//! keys are no longer re-emitted: they are passed through to the OS unchanged,
//! and only mapped keys are swallowed and re-emitted (via the emitter).  This
//! is what makes the two-process CGEventTap design possible — the capture
//! process never needs to re-inject unmapped keys.
//!
//! Unknown usages (raw HID codes that do not resolve to a [`HidUsage`]) are
//! filtered by the caller before [`DecisionContext::decide`] is invoked, so
//! the core only ever sees recognised keys.

use std::{collections::HashSet, sync::Arc};

use parking_lot::RwLock;

use crate::{
    common::hid_usage::HidUsage,
    daemon::{mapping_cache::NativeKey, state::Lookup},
};

/// The outcome of deciding how to handle a single key event.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// Let the event through unchanged.  No IPC, no emission.
    Pass,
    /// Swallow the event and emit these outputs via the emitter.
    Emit(Vec<NativeKey>),
    /// Swallow the event without emitting (a mapped key-up, or the auto-repeat
    /// of a mapped key).
    Swallow,
}

/// Per-keyboard decision state.
///
/// Holds the mutable bookkeeping that [`DecisionContext::decide`] needs to
/// turn a stream of raw key events into decisions: the held-modifier mask, the
/// set of keys currently down (for auto-repeat deduplication), and the set of
/// keys whose key-down was mapped (so their key-up is swallowed).  One context
/// is created per captured keyboard.
pub struct DecisionContext {
    /// Shared lookup for remapping rules.
    lookup: Arc<RwLock<dyn Lookup>>,
    /// HID modifier bits currently held.
    modifier_state: u8,
    /// Usage ids currently down (deduplication).
    pressed_keys: HashSet<u16>,
    /// Codes of keys whose key-down was mapped (their key-up is swallowed).
    mapped_keys: HashSet<u32>,
    /// Platform-specific device identifier for keyboard filtering.  `None`
    /// when the platform cannot identify the source keyboard (the case on
    /// macOS in the CGEventTap design).
    device_id: Option<&'static str>,
}

impl DecisionContext {
    /// Create a fresh decision context for a captured keyboard.
    pub fn new(
        lookup: Arc<RwLock<dyn Lookup>>,
        device_id: Option<&'static str>,
    ) -> Self {
        Self {
            lookup,
            modifier_state: 0,
            pressed_keys: HashSet::new(),
            mapped_keys: HashSet::new(),
            device_id,
        }
    }

    /// Decide how to handle a single key event.
    ///
    /// `hid_usage` is the HID identity of the key, `is_down` whether it is a
    /// key-down or key-up, and `reachable` whether the emitter (the process
    /// that re-emits mapped outputs) is currently reachable.  When the emitter
    /// is unreachable, every decision is [`Decision::Pass`] so typing keeps
    /// working natively and mappings are simply inactive.
    ///
    /// The bookkeeping is maintained regardless of `reachable`, so a key-up is
    /// swallowed only if its key-down was decided [`Decision::Emit`], no
    /// matter when the reachability flag flips.
    pub fn decide(
        &mut self,
        hid_usage: HidUsage,
        is_down: bool,
        reachable: bool,
    ) -> Decision {
        // Track pressed keys for deduplication.  Use the raw HID usage id
        // (page-specific, unambiguous).
        let key_id = hid_usage.id();

        if is_down {
            // Key-down.  An auto-repeat (the key is already tracked) is
            // swallowed only if the original key-down was mapped; otherwise it
            // passes through so the OS produces its native repeat.
            if !self.pressed_keys.insert(key_id) {
                return if self.mapped_keys.contains(&hid_usage.code()) {
                    Decision::Swallow
                } else {
                    Decision::Pass
                };
            }

            // Capture the modifier state before setting this key's own bit, so
            // a bare modifier trigger does not match itself.
            let lookup_modifiers = self.modifier_state;
            if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage) {
                self.modifier_state |= 1 << bit;
            }

            // If the emitter is unreachable, pass everything through natively.
            // The bookkeeping above is still maintained so the state stays
            // consistent when reachability returns.
            if !reachable {
                return Decision::Pass;
            }

            // Perform the lookup.  Compiled rules store the trigger as a
            // `HidUsage`, so the lookup is keyed by the full page-specific
            // usage.  App-scoped rules take precedence over global ones.
            let guard = self.lookup.read();
            let outputs = guard
                .for_active_app(hid_usage, lookup_modifiers, self.device_id)
                .or_else(|| {
                    guard.global(hid_usage, lookup_modifiers, self.device_id)
                })
                .map(|v| v.to_vec());
            drop(guard);

            match outputs {
                // Mapped: remember the key was mapped so its release is
                // swallowed, and emit the mapped outputs via the emitter.
                Some(outputs) => {
                    self.mapped_keys.insert(hid_usage.code());
                    Decision::Emit(outputs)
                }
                // Unmapped: let the event through unchanged.
                None => Decision::Pass,
            }
        } else {
            // Key-up.  Ignore releases for keys that were never tracked as
            // down (a stray up passes through).
            if !self.pressed_keys.remove(&key_id) {
                return Decision::Pass;
            }

            // Clear the modifier bit so subsequent lookups carry the correct
            // modifier state.
            if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage) {
                self.modifier_state &= !(1 << bit);
            }

            // A key whose key-down was mapped is swallowed on release; any
            // other key passes through.
            if self.mapped_keys.remove(&hid_usage.code()) {
                Decision::Swallow
            } else {
                Decision::Pass
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::test_lookup::TestLookup;

    /// Build a [`DecisionContext`] backed by a [`TestLookup`] compiled from
    /// the given YAML config.  The device id is `None`, matching the macOS
    /// CGEventTap design where the source keyboard cannot be identified.
    fn context(yaml: &str) -> DecisionContext {
        let lookup: Arc<RwLock<dyn Lookup>> =
            Arc::new(RwLock::new(TestLookup::from_yaml(yaml)));
        DecisionContext::new(lookup, None)
    }

    /// A single output key with no modifiers.
    fn key(usage: HidUsage) -> NativeKey {
        NativeKey {
            modifiers: 0,
            usage,
        }
    }

    #[test]
    fn unmapped_pass_through() {
        let mut ctx = context("- mappings:\n    CapsLock: LeftControl");
        // 'A' has no mapping, so it passes through unchanged on both edges.
        assert_eq!(ctx.decide(HidUsage::A, true, true), Decision::Pass);
        assert_eq!(ctx.decide(HidUsage::A, false, true), Decision::Pass);
    }

    #[test]
    fn simple_remap() {
        let mut ctx = context("- mappings:\n    A: B");
        // The key-down is mapped to 'B' and swallowed; the release is
        // swallowed so the OS never sees the original key.
        assert_eq!(
            ctx.decide(HidUsage::A, true, true),
            Decision::Emit(vec![key(HidUsage::B)])
        );
        assert_eq!(ctx.decide(HidUsage::A, false, true), Decision::Swallow);
    }

    #[test]
    fn modifier_only_trigger() {
        let mut ctx = context("- mappings:\n    RightAlt: LeftControl");
        // A bare modifier trigger fires with no other modifiers held.  The
        // lookup captures the modifier state before setting RightAlt's own
        // bit, so the trigger does not match itself.
        assert_eq!(
            ctx.decide(HidUsage::RightAlt, true, true),
            Decision::Emit(vec![key(HidUsage::LeftControl)])
        );
        assert_eq!(
            ctx.decide(HidUsage::RightAlt, false, true),
            Decision::Swallow
        );
    }

    #[test]
    fn chord_trigger() {
        let mut ctx = context("- mappings:\n    LeftShift+Backspace: Delete");
        // Bare Shift has no mapping, so it passes through.
        assert_eq!(
            ctx.decide(HidUsage::LeftShift, true, true),
            Decision::Pass
        );
        // With Shift held, Backspace fires the chord.
        assert_eq!(
            ctx.decide(HidUsage::Backspace, true, true),
            Decision::Emit(vec![key(HidUsage::Delete)])
        );
        // The chord's base key-up is swallowed; the modifier's release passes
        // through.
        assert_eq!(
            ctx.decide(HidUsage::Backspace, false, true),
            Decision::Swallow
        );
        assert_eq!(
            ctx.decide(HidUsage::LeftShift, false, true),
            Decision::Pass
        );
    }

    #[test]
    fn auto_repeat_of_mapped_key() {
        let mut ctx = context("- mappings:\n    A: B");
        // The first key-down is mapped.
        assert_eq!(
            ctx.decide(HidUsage::A, true, true),
            Decision::Emit(vec![key(HidUsage::B)])
        );
        // The auto-repeat of a mapped key is swallowed (the OS never saw the
        // original key-down, so it must not see the repeat either).
        assert_eq!(ctx.decide(HidUsage::A, true, true), Decision::Swallow);
        // The release is swallowed as well.
        assert_eq!(ctx.decide(HidUsage::A, false, true), Decision::Swallow);
    }

    #[test]
    fn stray_key_up() {
        let mut ctx = context("- mappings:\n    A: B");
        // A key-up with no prior key-down passes through.
        assert_eq!(ctx.decide(HidUsage::A, false, true), Decision::Pass);
    }

    #[test]
    fn emitter_unreachable_pass_through() {
        let mut ctx = context("- mappings:\n    A: B");
        // With the emitter unreachable, even a mapped key passes through so
        // typing keeps working natively.  The bookkeeping is still maintained,
        // so the release also passes through (the OS saw the key-down).
        assert_eq!(ctx.decide(HidUsage::A, true, false), Decision::Pass);
        assert_eq!(ctx.decide(HidUsage::A, false, false), Decision::Pass);
    }

    #[test]
    fn unreachable_down_then_reachable_up_stays_consistent() {
        let mut ctx = context("- mappings:\n    A: B");
        // The key-down happens while the emitter is unreachable, so it passes
        // through natively and is not recorded as mapped.
        assert_eq!(ctx.decide(HidUsage::A, true, false), Decision::Pass);
        // The emitter comes back before the release.  Because the key-down was
        // not mapped, the release passes through (the OS saw the key-down).
        assert_eq!(ctx.decide(HidUsage::A, false, true), Decision::Pass);
    }

    #[test]
    fn reachable_down_then_unreachable_up_is_swallowed() {
        let mut ctx = context("- mappings:\n    A: B");
        // The key-down is mapped and emitted while the emitter is reachable.
        assert_eq!(
            ctx.decide(HidUsage::A, true, true),
            Decision::Emit(vec![key(HidUsage::B)])
        );
        // The emitter dies before the release.  The key-up is still swallowed:
        // the OS never saw the key-down (it was emitted via the emitter), so
        // it must not see a stray release.
        assert_eq!(ctx.decide(HidUsage::A, false, false), Decision::Swallow);
    }
}
