// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The unified, platform-agnostic key-mapping engine.
//!
//! [`MappingEngine`] is the shared heart of every platform's capture path. It
//! owns all of the modifier-state bookkeeping that turns a stream of raw key
//! events into mapping decisions, so the three backends (Linux, macOS,
//! Windows) share one implementation instead of three divergent copies:
//!
//! - **pressed / swallowed keys** — a key-up's fate is decided from its
//!   key-down's own record, not from a re-run of the lookup (whose modifier
//!   state may have changed in the meantime, which would leak the release into
//!   the app as a phantom key-up, or swallow it while the key-down passed
//!   through and leave the key held).
//! - **forwarded / consumed modifier masks** — solve the "clean tap" problem:
//!   when a trigger fires while an unmapped modifier is held, that modifier is
//!   released on the output device first (and marked consumed so its physical
//!   release is swallowed), so the emitted output is not an unintended chord.
//! - **held output modifiers** — a rule whose output is a modifier key holds
//!   it on the output device until the physical key-up, so a remapped modifier
//!   stays active for the key presses that follow it.
//!
//! The engine is generic over the platform's key identity `K` (any `Ord` type:
//! an evdev code on Linux, a `(scan, ext)` pair on Windows, a HID usage id on
//! macOS). It depends only on the shared [`HidUsage`], [`NativeKey`], and
//! [`Lookup`] types, so it is unit-testable in isolation with the
//! [`TestLookup`](crate::daemon::test_lookup::TestLookup) harness.
//!
//! [`MappingEngine::decide`] returns a [`Decision`] that the platform's
//! emission layer interprets according to its own architecture. The `release`
//! mask on [`Decision::Emit`] and [`Decision::Swallow`] is the clean-tap /
//! held-output mechanism; a platform whose output device does not need it
//! (macOS, where the virtual keyboard's modifier state is isolated from
//! physical typing) simply ignores it.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use parking_lot::RwLock;

use crate::{
    common::hid_usage::HidUsage,
    daemon::{mapping_cache::NativeKey, state::Lookup},
};

/// The outcome of deciding how to handle a single key event.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// Let the event through unchanged. No IPC, no emission.
    Pass,
    /// Swallow the event and emit these outputs via the platform emitter.
    ///
    /// `release` is the mask of modifier bits the emitter must release on the
    /// output device before emitting, so the output is a clean tap (the
    /// trigger's forwarded or held modifiers do not leak into the chord).
    Emit {
        release: u8,
        outputs: Vec<NativeKey>,
    },
    /// Swallow the event without emitting (a mapped key-up, a consumed
    /// modifier release, or the auto-repeat of a mapped key).
    ///
    /// `release` is the mask of held output modifier bits to release on the
    /// output device (0 when none).
    Swallow { release: u8 },
}

/// Per-keyboard key-fate and modifier bookkeeping.
///
/// Kept separate from the lookup so the swallow/forward decision is
/// unit-testable in isolation. The decisive fact is that a key-up is decided
/// from its key-down's own record rather than from a re-run of the lookup.
struct KeyTracker<K: Ord> {
    /// Keys currently down (auto-repeat deduplication).
    pressed_keys: BTreeSet<K>,
    /// Keys whose key-down fired a mapped trigger and was swallowed. Their
    /// key-ups are swallowed unconditionally, regardless of the modifier
    /// state at release time.
    swallowed_keys: BTreeSet<K>,
    /// Swallowed key-downs whose mapped output is a modifier key, with the
    /// mask of the modifier bits that are therefore held on the output
    /// device. The bits are released when the physical key is released,
    /// or when a later fired trigger consumes them.
    held_output_modifiers: BTreeMap<K, u8>,
    /// Bitmask of forwarded (unmapped) modifier keys that are still held on
    /// the output device.
    forwarded_modifiers: u8,
    /// Bitmask of modifier keys that were part of a fired trigger and have
    /// already been released on the output device. Their physical release is
    /// swallowed so it is not forwarded a second time.
    consumed_modifiers: u8,
}

impl<K: Ord> Default for KeyTracker<K> {
    fn default() -> Self {
        Self {
            pressed_keys: BTreeSet::new(),
            swallowed_keys: BTreeSet::new(),
            held_output_modifiers: BTreeMap::new(),
            forwarded_modifiers: 0,
            consumed_modifiers: 0,
        }
    }
}

impl<K: Ord> KeyTracker<K> {
    /// Track a forwarded (unmapped) modifier press. A fresh press clears any
    /// stale consumed mark, since the earlier release belonged to the previous
    /// press.
    fn record_forwarded_down(&mut self, bit: u8) {
        let mask = 1u8 << bit;
        self.forwarded_modifiers |= mask;
        self.consumed_modifiers &= !mask;
    }

    /// Consume the modifiers of a fired trigger and return `(released, held)`:
    /// *released* is the mask the emitter must release on the output device —
    /// the forwarded subset (moved into the consumed mask, so its physical
    /// release is swallowed) plus any held output modifier that was part of
    /// the trigger — and *held* is the subset of that mask that came from
    /// held outputs, so the caller can clear those bits from the lookup
    /// modifier state.
    fn consume_triggered(&mut self, modifiers: u8) -> (u8, u8) {
        let forwarded = modifiers & self.forwarded_modifiers;
        self.forwarded_modifiers &= !forwarded;
        self.consumed_modifiers |= forwarded;

        let mut held = 0u8;
        self.held_output_modifiers.retain(|_, mask| {
            let hit = *mask & modifiers;
            held |= hit;
            *mask &= !hit;
            *mask != 0
        });

        (forwarded | held, held)
    }

    /// Record that a swallowed key-down's mapped output holds modifier keys on
    /// the output device. The bits are released when the physical key is
    /// released, or when a later fired trigger consumes them.
    fn hold_output_modifiers(&mut self, key: K, mask: u8) {
        if mask != 0 {
            self.held_output_modifiers
                .entry(key)
                .and_modify(|m| *m |= mask)
                .or_insert(mask);
        }
    }

    /// Decide the fate of a key-up. Returns `Some(mask)` when the release is
    /// swallowed (its key-down fired a trigger, or the modifier was consumed
    /// by one), with *mask* the held output modifier bits to release on
    /// the output device (0 when none). Returns `None` when the release is
    /// forwarded; for a forwarded modifier the tracking bit is cleared.
    fn release(&mut self, key: K, usage: HidUsage) -> Option<u8> {
        if self.swallowed_keys.remove(&key) {
            return Some(self.held_output_modifiers.remove(&key).unwrap_or(0));
        }
        if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage) {
            let mask = 1u8 << bit;
            if self.consumed_modifiers & mask != 0 {
                self.consumed_modifiers &= !mask;
                return Some(0);
            }
            self.forwarded_modifiers &= !mask;
        }
        None
    }
}

/// The unified mapping engine: one instance per captured keyboard (or one
/// global instance on platforms that cannot identify the source keyboard).
pub struct MappingEngine<K: Ord + Copy> {
    /// Shared lookup for remapping rules.
    lookup: Arc<RwLock<dyn Lookup>>,
    /// Bitmask of modifier bits currently active for rule lookup: the
    /// physical modifiers plus any held output modifiers.
    modifier_state: u8,
    /// Key-fate and forwarded/consumed modifier tracking.
    tracker: KeyTracker<K>,
}

impl<K: Ord + Copy> MappingEngine<K> {
    /// Create a fresh engine backed by the given lookup.
    pub fn new(lookup: Arc<RwLock<dyn Lookup>>) -> Self {
        Self {
            lookup,
            modifier_state: 0,
            tracker: KeyTracker::default(),
        }
    }

    /// Note a key that is already held before the event stream started (e.g.
    /// held at device-grab time on Linux, where grabbing delivers events only
    /// from the grab onward).
    ///
    /// The key is marked pressed so its later release and any repeats are not
    /// treated as a fresh press, and — for a modifier — its bit is set in the
    /// lookup state and marked forwarded so the release is forwarded (not
    /// swallowed) to the output device. This maintains the invariant that a
    /// set modifier bit always has its key in the pressed set.
    pub fn note_held_key(&mut self, key: K, usage: HidUsage) {
        self.tracker.pressed_keys.insert(key);
        if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage) {
            self.modifier_state |= 1 << bit;
            self.tracker.record_forwarded_down(bit);
        }
    }

    /// Decide how to handle a single key event.
    ///
    /// `key` is the platform's identity for the physical key, `usage` its HID
    /// identity (the lookup key space), `is_down` whether it is a key-down or
    /// key-up, `device_id` an optional platform-specific device identifier for
    /// keyboard filtering (`None` when the platform cannot identify the source
    /// keyboard), and `reachable` whether the emitter is currently reachable.
    /// When the emitter is unreachable, every mapped key-down passes through
    /// natively so typing keeps working and mappings are simply inactive.
    ///
    /// The bookkeeping is maintained regardless of `reachable`, so a key-up is
    /// swallowed only if its key-down was decided [`Decision::Emit`], no
    /// matter when the reachability flag flips.
    pub fn decide(
        &mut self,
        key: K,
        usage: HidUsage,
        is_down: bool,
        device_id: Option<&str>,
        reachable: bool,
    ) -> Decision {
        if is_down {
            self.decide_down(key, usage, device_id, reachable)
        } else {
            self.decide_up(key, usage)
        }
    }

    /// Decide the fate of a key-down (or its auto-repeat).
    fn decide_down(
        &mut self,
        key: K,
        usage: HidUsage,
        device_id: Option<&str>,
        reachable: bool,
    ) -> Decision {
        // An auto-repeat (the key is already tracked) is swallowed only if the
        // original key-down was mapped; otherwise it passes through so the OS
        // produces its native repeat.
        if !self.tracker.pressed_keys.insert(key) {
            return if self.tracker.swallowed_keys.contains(&key) {
                Decision::Swallow { release: 0 }
            } else {
                Decision::Pass
            };
        }

        // Capture the modifier state before setting this key's own bit, so a
        // bare-modifier trigger does not match itself.
        let lookup_modifiers = self.modifier_state;
        if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage) {
            self.modifier_state |= 1 << bit;
        }

        // If the emitter is unreachable, pass everything through natively. The
        // bookkeeping above is still maintained so the state stays consistent
        // when reachability returns.
        if !reachable {
            return Decision::Pass;
        }

        // Compiled rules store the trigger as a `HidUsage`, so the lookup is
        // keyed by the full page-specific usage. App-scoped rules take
        // precedence over global ones.
        let guard = self.lookup.read();
        let outputs = guard
            .for_active_app(usage, lookup_modifiers, device_id)
            .or_else(|| guard.global(usage, lookup_modifiers, device_id))
            .map(|v| v.to_vec());
        drop(guard);

        match outputs {
            // Mapped: remember the key was mapped so its release is swallowed,
            // and emit the mapped outputs via the emitter.
            Some(outputs) => {
                self.tracker.swallowed_keys.insert(key);

                // The trigger's modifiers were forwarded when pressed (or are
                // held by another remapped key's modifier output). Release
                // them now so the output is emitted as a clean
                // tap; forwarded marks are consumed so their
                // physical release is swallowed, and held bits
                // are cleared from the lookup state since no physical key
                // tracks them.
                let (consumed, held_consumed) =
                    self.tracker.consume_triggered(lookup_modifiers);
                self.modifier_state &= !held_consumed;

                // If the physical key is itself a modifier, its bit was set
                // above for the pre-update lookup; it is mapped, not
                // forwarded, so clear it again.
                if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage) {
                    self.modifier_state &= !(1 << bit);
                }

                // An output whose base is itself a modifier key is held down
                // on the output device (not tapped) so the
                // remapped modifier stays active for
                // subsequent key presses; the matching release is
                // emitted when the physical key-up arrives.
                let mut held_mask: u8 = 0;
                for native_key in &outputs {
                    if let Some(mask) = output_held_mask(native_key) {
                        self.tracker.hold_output_modifiers(key, mask);
                        held_mask |= mask;
                    }
                }
                if held_mask != 0 {
                    self.modifier_state |= held_mask;
                }

                Decision::Emit {
                    release: consumed,
                    outputs,
                }
            }
            // Unmapped: track a forwarded modifier press so a later fired
            // trigger can release it cleanly, then let the event through.
            None => {
                if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage) {
                    self.tracker.record_forwarded_down(bit);
                }
                Decision::Pass
            }
        }
    }

    /// Decide the fate of a key-up.
    fn decide_up(&mut self, key: K, usage: HidUsage) -> Decision {
        // Ignore releases for keys that were never tracked as down (a stray up
        // passes through).
        if !self.tracker.pressed_keys.remove(&key) {
            return Decision::Pass;
        }

        // Clear this key's own modifier bit so subsequent lookups carry the
        // correct modifier state.
        if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage) {
            self.modifier_state &= !(1 << bit);
        }

        // A key whose key-down was mapped (or a consumed modifier) is
        // swallowed on release; any other key passes through. For a
        // swallowed key whose output held modifier bits, those bits
        // are released now and cleared from the lookup state.
        match self.tracker.release(key, usage) {
            Some(release) => {
                if release != 0 {
                    self.modifier_state &= !release;
                }
                Decision::Swallow { release }
            }
            None => Decision::Pass,
        }
    }
}

/// If the output key's base is itself a modifier key, return the mask of
/// modifier bits to hold on the output device while the physical key is
/// pressed: the base's own bit plus any of the output's modifier bits. Returns
/// `None` for regular keys, which are emitted as taps.
pub(crate) fn output_held_mask(native_key: &NativeKey) -> Option<u8> {
    let bit = HidUsage::hid_usage_to_modifier_bit(native_key.usage)?;
    Some((1u8 << bit) | native_key.modifiers)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::test_lookup::TestLookup;

    // -----------------------------------------------------------------------
    // Key-fate tracking tests (tracker level)
    // -----------------------------------------------------------------------
    //
    // Verifies that a key-up's fate is decided from the key-down's own record
    // (and the forwarded/consumed modifier state) rather than from a re-run of
    // the lookup, whose modifier state may have changed in the meantime. These
    // are the consolidated bookkeeping tests of the Linux, macOS, and Windows
    // backends.

    /// HID usage id of `LeftControl` (bit 0).
    const CTRL: u16 = 0xE0;
    /// HID usage id of `LeftShift` (bit 1).
    const SHIFT: u16 = 0xE1;
    /// HID usage id of `LeftAlt` (bit 2).
    const ALT: u16 = 0xE2;
    /// HID usage id of `LeftCommand` (bit 3).
    const META: u16 = 0xE3;
    /// HID usage id of `RightShift` (bit 5).
    const RSHIFT: u16 = 0xE5;
    /// HID usage id of `A`.
    const A: u16 = 0x04;
    /// HID usage id of `CapsLock`.
    const CAPS: u16 = 0x39;
    /// HID usage id of `F1`, used as a second physical key.
    const F1: u16 = 0x3A;

    #[test]
    fn swallowed_key_down_swallows_its_key_up() {
        let mut t = KeyTracker::<u16>::default();
        // The key-down fired a mapped trigger and was swallowed.
        t.swallowed_keys.insert(A);
        // Its key-up is swallowed regardless of the modifier state, and the
        // record is consumed.
        assert_eq!(t.release(A, HidUsage::A), Some(0));
        assert_eq!(t.release(A, HidUsage::A), None);
    }

    #[test]
    fn mapped_base_release_swallowed_after_modifier_state_change() {
        // Models `Ctrl+Semicolon -> C` where the modifier is released before
        // the base. The base's key-down fired the trigger (recorded); its
        // key-up arrives after the modifier state changed (Ctrl released), so
        // a re-derived lookup would not match and the release would
        // leak as a phantom key-up. The record keeps it swallowed.
        let mut t = KeyTracker::<u16>::default();

        // Ctrl down: forwarded (unmapped).
        t.record_forwarded_down(0);
        // Semicolon (base) down: fires the trigger. Consume the held Ctrl and
        // record the base.
        assert_eq!(t.consume_triggered(1), (1, 0));
        t.swallowed_keys.insert(A);

        // Ctrl up: consumed by the trigger, so swallowed.
        assert_eq!(t.release(CTRL, HidUsage::LeftControl), Some(0));

        // Base up: modifier state is now empty, but the base's key-down fired
        // a trigger, so its release is swallowed (not a phantom
        // key-up).
        assert_eq!(t.release(A, HidUsage::A), Some(0));
    }

    #[test]
    fn forwarded_modifier_key_up_forwards_and_untracks() {
        let mut t = KeyTracker::<u16>::default();
        t.record_forwarded_down(0);
        // Its release is forwarded and the tracking bit is cleared.
        assert_eq!(t.release(CTRL, HidUsage::LeftControl), None);
    }

    #[test]
    fn consumed_modifier_key_up_swallowed() {
        let mut t = KeyTracker::<u16>::default();
        // Ctrl is forwarded, then consumed by a fired trigger.
        t.record_forwarded_down(0);
        assert_eq!(t.consume_triggered(1), (1, 0));
        // Its release is swallowed (already released on the output device).
        assert_eq!(t.release(CTRL, HidUsage::LeftControl), Some(0));
    }

    #[test]
    fn fresh_forwarded_press_clears_stale_consumed_mark() {
        let mut t = KeyTracker::<u16>::default();
        // Ctrl forwarded, then consumed by a trigger; its release is swallowed
        // by another path, leaving the consumed mark stale.
        t.record_forwarded_down(0);
        assert_eq!(t.consume_triggered(1), (1, 0));
        // A fresh Ctrl press must clear the stale mark so its release forwards
        // rather than being wrongly swallowed (which would leave it stuck).
        t.record_forwarded_down(0);
        assert_eq!(t.release(CTRL, HidUsage::LeftControl), None);
    }

    // -----------------------------------------------------------------------
    // Remapped-modifier (held output) tests
    // -----------------------------------------------------------------------
    //
    // Verifies that a rule whose output is a modifier key holds that modifier
    // on the output device until the physical key-up, so the remapped modifier
    // stays active for subsequent key presses.

    #[test]
    fn modifier_output_is_held_until_physical_release() {
        // Models `CapsLock: LeftControl`: the key-down fires the trigger and
        // holds the output modifier; the physical key-up is swallowed and
        // returns the held mask for release.
        let mut t = KeyTracker::<u16>::default();
        t.swallowed_keys.insert(CAPS);
        t.hold_output_modifiers(CAPS, 1); // LeftControl bit.

        assert_eq!(t.release(CAPS, HidUsage::CapsLock), Some(1));
        // A second release finds no record and is forwarded.
        assert_eq!(t.release(CAPS, HidUsage::CapsLock), None);
    }

    #[test]
    fn two_remapped_modifiers_held_independently() {
        // Two physical keys remapped to different modifiers: releasing one
        // must not affect the other.
        let mut t = KeyTracker::<u16>::default();
        t.swallowed_keys.insert(CAPS);
        t.hold_output_modifiers(CAPS, 1); // LeftControl
        t.swallowed_keys.insert(F1);
        t.hold_output_modifiers(F1, 2); // LeftShift

        assert_eq!(t.release(CAPS, HidUsage::CapsLock), Some(1));
        assert_eq!(t.release(F1, HidUsage::F1), Some(2));
    }

    #[test]
    fn fired_trigger_consumes_held_output_modifier() {
        // Models `CapsLock: LeftControl` (held) followed by `Ctrl+Base: X`
        // fired while the remapped Ctrl is still held: the held bit is
        // released with the trigger's modifiers and removed from the
        // map, so the physical CapsLock key-up releases nothing.
        let mut t = KeyTracker::<u16>::default();
        t.swallowed_keys.insert(CAPS);
        t.hold_output_modifiers(CAPS, 1);

        assert_eq!(t.consume_triggered(1), (1, 1));
        assert_eq!(t.release(CAPS, HidUsage::CapsLock), Some(0));
    }

    #[test]
    fn consume_triggered_partial_held_mask_keeps_remaining_bits() {
        // A held output with two modifier bits consumed by a trigger that
        // carries only one of them: that bit is released and the other stays
        // held for the physical key-up.
        let mut t = KeyTracker::<u16>::default();
        t.swallowed_keys.insert(CAPS);
        t.hold_output_modifiers(CAPS, 3); // LeftControl | LeftShift

        assert_eq!(t.consume_triggered(1), (1, 1));
        assert_eq!(t.release(CAPS, HidUsage::CapsLock), Some(2));
    }

    #[test]
    fn output_held_mask_covers_modifier_bases() {
        // A regular key is a tap (no held bits); a modifier base holds its own
        // bit plus the output's modifier bits.
        assert_eq!(
            output_held_mask(&NativeKey {
                modifiers: 0,
                usage: HidUsage::A,
            }),
            None
        );
        assert_eq!(
            output_held_mask(&NativeKey {
                modifiers: 0,
                usage: HidUsage::LeftControl,
            }),
            Some(1)
        );
        assert_eq!(
            output_held_mask(&NativeKey {
                modifiers: 1,
                usage: HidUsage::LeftShift,
            }),
            Some(3)
        );
    }

    // -----------------------------------------------------------------------
    // Forwarded-modifier state tests (consolidated from the Windows backend)
    // -----------------------------------------------------------------------

    #[test]
    fn forwarded_down_up_round_trip() {
        // A forwarded (pass-through) modifier press is tracked, and its
        // physical release is forwarded (not swallowed).
        let mut t = KeyTracker::<u16>::default();
        t.record_forwarded_down(0);
        assert_eq!(t.forwarded_modifiers, 0b0000_0001);
        assert_eq!(t.release(CTRL, HidUsage::LeftControl), None);
        assert_eq!(t.forwarded_modifiers, 0);
        assert_eq!(t.consumed_modifiers, 0);
    }

    #[test]
    fn forwarded_consume_releases_and_swallows_releases() {
        // A trigger firing while both modifiers are held consumes them; their
        // physical releases are then swallowed.
        let mut t = KeyTracker::<u16>::default();
        t.record_forwarded_down(0);
        t.record_forwarded_down(1);

        let (released, held) = t.consume_triggered(0b0000_0011);
        assert_eq!(released, 0b0000_0011);
        assert_eq!(held, 0);
        assert_eq!(t.forwarded_modifiers, 0);
        assert_eq!(t.consumed_modifiers, 0b0000_0011);

        assert_eq!(t.release(CTRL, HidUsage::LeftControl), Some(0));
        assert_eq!(t.release(SHIFT, HidUsage::LeftShift), Some(0));
        assert_eq!(t.consumed_modifiers, 0);
    }

    #[test]
    fn forwarded_consume_partial_subset() {
        // Only the modifiers that were actually forwarded are consumed; the
        // others are untouched and their releases still forward.
        let mut t = KeyTracker::<u16>::default();
        t.record_forwarded_down(2);

        let (released, _) = t.consume_triggered(0b0000_1100);
        assert_eq!(released, 0b0000_0100);
        assert_eq!(t.forwarded_modifiers, 0);
        assert_eq!(t.consumed_modifiers, 0b0000_0100);

        assert_eq!(t.release(ALT, HidUsage::LeftAlt), Some(0)); // consumed
        assert_eq!(t.release(META, HidUsage::LeftCommand), None); // never forwarded
    }

    #[test]
    fn forwarded_consume_without_forwarded_modifiers() {
        // A trigger firing with no forwarded modifiers consumes nothing.
        let mut t = KeyTracker::<u16>::default();
        let (released, held) = t.consume_triggered(0b0000_0011);
        assert_eq!(released, 0);
        assert_eq!(held, 0);
        assert_eq!(t.forwarded_modifiers, 0);
        assert_eq!(t.consumed_modifiers, 0);
    }

    #[test]
    fn forwarded_late_release_after_consume_is_forwarded() {
        // A modifier pressed again after being consumed (a fresh physical
        // press) is tracked as forwarded once more, so its release forwards
        // instead of being swallowed.
        let mut t = KeyTracker::<u16>::default();
        t.record_forwarded_down(0);
        let (released, _) = t.consume_triggered(0b0000_0001);
        assert_eq!(released, 0b0000_0001);
        t.record_forwarded_down(0);
        assert_eq!(t.release(CTRL, HidUsage::LeftControl), None);
        assert_eq!(t.forwarded_modifiers, 0);
        assert_eq!(t.consumed_modifiers, 0);
    }

    #[test]
    fn release_swallows_release_of_mapped_key() {
        // A release of a key whose key-down fired a mapped trigger is
        // swallowed and clears the record.
        let mut t = KeyTracker::<u16>::default();
        t.swallowed_keys.insert(A);
        assert_eq!(t.release(A, HidUsage::A), Some(0));
        assert!(t.swallowed_keys.is_empty());
    }

    #[test]
    fn release_forwards_release_of_unmapped_key() {
        // A release without a swallowed key-down record forwards.
        let mut t = KeyTracker::<u16>::default();
        assert_eq!(t.release(A, HidUsage::A), None);
    }

    #[test]
    fn release_swallows_consumed_modifier_release() {
        // A modifier consumed by a fired trigger swallows its physical
        // release; a plain forwarded modifier's release forwards and
        // untracks.
        let mut t = KeyTracker::<u16>::default();
        t.record_forwarded_down(1); // LeftShift
        t.record_forwarded_down(5); // RightShift
        let (released, _) = t.consume_triggered(0b0000_0010);
        assert_eq!(released, 0b0000_0010);

        // LeftShift was consumed: swallow.
        assert_eq!(t.release(SHIFT, HidUsage::LeftShift), Some(0));
        // RightShift was only forwarded: forward and untrack.
        assert_eq!(t.release(RSHIFT, HidUsage::RightShift), None);
        assert_eq!(t.forwarded_modifiers, 0);
        assert_eq!(t.consumed_modifiers, 0);
    }

    // -----------------------------------------------------------------------
    // Engine-level tests (full `decide` path, consolidated from the macOS
    // backend plus clean-tap and held-output cases)
    // -----------------------------------------------------------------------

    /// Build a [`MappingEngine<u16>`] backed by a [`TestLookup`] compiled from
    /// the given YAML config. The key identity is the HID usage id, matching
    /// the macOS design where the source keyboard cannot be identified.
    fn engine(yaml: &str) -> MappingEngine<u16> {
        let lookup: Arc<RwLock<dyn Lookup>> =
            Arc::new(RwLock::new(TestLookup::from_yaml(yaml)));
        MappingEngine::new(lookup)
    }

    /// A single output key with no modifiers.
    fn nk(usage: HidUsage) -> NativeKey {
        NativeKey {
            modifiers: 0,
            usage,
        }
    }

    /// Decide a key-down with the emitter reachable and no device id.
    fn down(e: &mut MappingEngine<u16>, usage: HidUsage) -> Decision {
        e.decide(usage.id(), usage, true, None, true)
    }

    /// Decide a key-up with the emitter reachable and no device id.
    fn up(e: &mut MappingEngine<u16>, usage: HidUsage) -> Decision {
        e.decide(usage.id(), usage, false, None, true)
    }

    /// Decide a key-down with the emitter unreachable.
    fn down_unreachable(
        e: &mut MappingEngine<u16>,
        usage: HidUsage,
    ) -> Decision {
        e.decide(usage.id(), usage, true, None, false)
    }

    /// Decide a key-up with the emitter unreachable.
    fn up_unreachable(
        e: &mut MappingEngine<u16>,
        usage: HidUsage,
    ) -> Decision {
        e.decide(usage.id(), usage, false, None, false)
    }

    #[test]
    fn unmapped_pass_through() {
        let mut e = engine("- mappings:\n    CapsLock: LeftControl");
        // 'A' has no mapping, so it passes through unchanged on both edges.
        assert_eq!(down(&mut e, HidUsage::A), Decision::Pass);
        assert_eq!(up(&mut e, HidUsage::A), Decision::Pass);
    }

    #[test]
    fn simple_remap() {
        let mut e = engine("- mappings:\n    A: B");
        // The key-down is mapped to 'B' and swallowed; the release is
        // swallowed so the OS never sees the original key.
        assert_eq!(
            down(&mut e, HidUsage::A),
            Decision::Emit {
                release: 0,
                outputs: vec![nk(HidUsage::B)]
            }
        );
        assert_eq!(up(&mut e, HidUsage::A), Decision::Swallow { release: 0 });
    }

    #[test]
    fn modifier_only_trigger_holds_its_output() {
        let mut e = engine("- mappings:\n    RightAlt: LeftControl");
        // A bare modifier trigger fires with no other modifiers held. The
        // lookup captures the modifier state before setting RightAlt's own
        // bit, so the trigger does not match itself. The output is a
        // modifier base, so it is held (not tapped) and released on
        // the physical key-up.
        assert_eq!(
            down(&mut e, HidUsage::RightAlt),
            Decision::Emit {
                release: 0,
                outputs: vec![nk(HidUsage::LeftControl)]
            }
        );
        assert_eq!(
            up(&mut e, HidUsage::RightAlt),
            Decision::Swallow { release: 1 }
        );
    }

    #[test]
    fn chord_trigger_is_a_clean_tap() {
        let mut e = engine("- mappings:\n    LeftShift+Backspace: Delete");
        // Bare Shift has no mapping, so it passes through (and is tracked as
        // forwarded).
        assert_eq!(down(&mut e, HidUsage::LeftShift), Decision::Pass);
        // With Shift held, Backspace fires the chord. The forwarded Shift is
        // released first (the `release` mask) so the output is a clean tap.
        assert_eq!(
            down(&mut e, HidUsage::Backspace),
            Decision::Emit {
                release: 2,
                outputs: vec![nk(HidUsage::Delete)]
            }
        );
        // The chord's base key-up is swallowed.
        assert_eq!(
            up(&mut e, HidUsage::Backspace),
            Decision::Swallow { release: 0 }
        );
        // The modifier was consumed by the trigger, so its release is
        // swallowed rather than forwarded a second time.
        assert_eq!(
            up(&mut e, HidUsage::LeftShift),
            Decision::Swallow { release: 0 }
        );
    }

    #[test]
    fn auto_repeat_of_mapped_key() {
        let mut e = engine("- mappings:\n    A: B");
        // The first key-down is mapped.
        assert_eq!(
            down(&mut e, HidUsage::A),
            Decision::Emit {
                release: 0,
                outputs: vec![nk(HidUsage::B)]
            }
        );
        // The auto-repeat of a mapped key is swallowed (the OS never saw the
        // original key-down, so it must not see the repeat either).
        assert_eq!(
            down(&mut e, HidUsage::A),
            Decision::Swallow { release: 0 }
        );
        // The release is swallowed as well.
        assert_eq!(up(&mut e, HidUsage::A), Decision::Swallow { release: 0 });
    }

    #[test]
    fn stray_key_up() {
        let mut e = engine("- mappings:\n    A: B");
        // A key-up with no prior key-down passes through.
        assert_eq!(up(&mut e, HidUsage::A), Decision::Pass);
    }

    #[test]
    fn synced_held_modifier_is_active_for_lookup() {
        // A modifier held at grab time is noted as held: its bit is active
        // for rule lookup, so a chord pressed while it is held fires. The
        // modifier was forwarded (not mapped), so the trigger consumes it and
        // its physical release is swallowed.
        let mut e = engine("- mappings:\n    LeftControl+A: B");
        e.note_held_key(HidUsage::LeftControl.id(), HidUsage::LeftControl);
        assert_eq!(
            down(&mut e, HidUsage::A),
            Decision::Emit {
                release: 1,
                outputs: vec![nk(HidUsage::B)]
            }
        );
        assert_eq!(up(&mut e, HidUsage::A), Decision::Swallow { release: 0 });
        assert_eq!(
            up(&mut e, HidUsage::LeftControl),
            Decision::Swallow { release: 0 }
        );
    }

    #[test]
    fn synced_held_modifier_release_forwards_when_not_consumed() {
        // A modifier held at grab time and never consumed by a trigger is
        // forwarded on release (the output device received its press during
        // the initial-state sync).
        let mut e = engine("- mappings:\n    CapsLock: LeftControl");
        e.note_held_key(HidUsage::LeftControl.id(), HidUsage::LeftControl);
        assert_eq!(up(&mut e, HidUsage::LeftControl), Decision::Pass);
    }

    #[test]
    fn synced_held_key_repeat_is_not_a_fresh_press() {
        // A key held at grab time is noted as pressed, so a repeat of it is
        // not re-decided as a fresh key-down (which would fire a mapping that
        // did not exist when the key went down).
        let mut e = engine("- mappings:\n    A: B");
        e.note_held_key(HidUsage::A.id(), HidUsage::A);
        assert_eq!(down(&mut e, HidUsage::A), Decision::Pass);
        assert_eq!(up(&mut e, HidUsage::A), Decision::Pass);
    }

    #[test]
    fn emitter_unreachable_pass_through() {
        let mut e = engine("- mappings:\n    A: B");
        // With the emitter unreachable, even a mapped key passes through so
        // typing keeps working natively. The bookkeeping is still maintained,
        // so the release also passes through (the OS saw the key-down).
        assert_eq!(down_unreachable(&mut e, HidUsage::A), Decision::Pass);
        assert_eq!(up_unreachable(&mut e, HidUsage::A), Decision::Pass);
    }

    #[test]
    fn unreachable_down_then_reachable_up_stays_consistent() {
        let mut e = engine("- mappings:\n    A: B");
        // The key-down happens while the emitter is unreachable, so it passes
        // through natively and is not recorded as mapped.
        assert_eq!(down_unreachable(&mut e, HidUsage::A), Decision::Pass);
        // The emitter comes back before the release. Because the key-down was
        // not mapped, the release passes through (the OS saw the key-down).
        assert_eq!(up(&mut e, HidUsage::A), Decision::Pass);
    }

    #[test]
    fn reachable_down_then_unreachable_up_is_swallowed() {
        let mut e = engine("- mappings:\n    A: B");
        // The key-down is mapped and emitted while the emitter is reachable.
        assert_eq!(
            down(&mut e, HidUsage::A),
            Decision::Emit {
                release: 0,
                outputs: vec![nk(HidUsage::B)]
            }
        );
        // The emitter dies before the release. The key-up is still swallowed:
        // the OS never saw the key-down (it was emitted via the emitter), so
        // it must not see a stray release.
        assert_eq!(
            up_unreachable(&mut e, HidUsage::A),
            Decision::Swallow { release: 0 }
        );
    }

    // -----------------------------------------------------------------------
    // Clean-tap and held-output integration cases
    // -----------------------------------------------------------------------

    #[test]
    fn clean_tap_releases_forwarded_trigger_modifiers() {
        // Models `Ctrl+Semicolon -> C`. Ctrl is forwarded when pressed; when
        // Semicolon fires the trigger, the held Ctrl is released (clean tap)
        // and marked consumed so its physical release is swallowed.
        let mut e = engine("- mappings:\n    LeftControl+Semicolon: C");

        // Ctrl down: unmapped, forwarded.
        assert_eq!(down(&mut e, HidUsage::LeftControl), Decision::Pass);

        // Semicolon down: fires the trigger. The Emit releases the forwarded
        // Ctrl (bit 0) so the output 'C' is a clean tap.
        assert_eq!(
            down(&mut e, HidUsage::Semicolon),
            Decision::Emit {
                release: 1,
                outputs: vec![nk(HidUsage::C)]
            }
        );

        // Semicolon up: swallowed (its key-down fired the trigger).
        assert_eq!(
            up(&mut e, HidUsage::Semicolon),
            Decision::Swallow { release: 0 }
        );

        // Ctrl up: consumed by the trigger, so swallowed (not forwarded
        // twice).
        assert_eq!(
            up(&mut e, HidUsage::LeftControl),
            Decision::Swallow { release: 0 }
        );
    }

    #[test]
    fn held_modifier_output_released_on_physical_release() {
        // Models `CapsLock: LeftControl`: the key-down holds the output
        // modifier on the output device; the physical key-up releases
        // it.
        let mut e = engine("- mappings:\n    CapsLock: LeftControl");

        assert_eq!(
            down(&mut e, HidUsage::CapsLock),
            Decision::Emit {
                release: 0,
                outputs: vec![nk(HidUsage::LeftControl)]
            }
        );

        // CapsLock up: releases the held LeftControl bit.
        assert_eq!(
            up(&mut e, HidUsage::CapsLock),
            Decision::Swallow { release: 1 }
        );
    }

    #[test]
    fn held_modifier_output_stays_active_for_subsequent_keys() {
        // Models `CapsLock: LeftControl` (remap CapsLock to hold Ctrl)
        // followed by `Ctrl+A: B`. While the remapped Ctrl is held, A
        // fires the Ctrl+A rule; the held bit is consumed by that
        // trigger (released for a clean tap), so the physical CapsLock
        // key-up releases nothing.
        let mut e = engine(
            "- mappings:\n    CapsLock: LeftControl\n    LeftControl+A: B",
        );

        // CapsLock down: remapped to hold LeftControl.
        assert_eq!(
            down(&mut e, HidUsage::CapsLock),
            Decision::Emit {
                release: 0,
                outputs: vec![nk(HidUsage::LeftControl)]
            }
        );

        // With the remapped Ctrl held, A fires the Ctrl+A rule. The held Ctrl
        // is consumed by the trigger (released for a clean tap).
        assert_eq!(
            down(&mut e, HidUsage::A),
            Decision::Emit {
                release: 1,
                outputs: vec![nk(HidUsage::B)]
            }
        );
        assert_eq!(up(&mut e, HidUsage::A), Decision::Swallow { release: 0 });

        // CapsLock up: its held output was already consumed, so it releases
        // nothing.
        assert_eq!(
            up(&mut e, HidUsage::CapsLock),
            Decision::Swallow { release: 0 }
        );
    }

    #[test]
    fn modifier_state_is_isolated_per_engine() {
        // Each engine instance tracks its own modifier state: holding a
        // modifier on one engine must not affect another's lookup.
        let yaml = "- mappings:\n    LeftControl+A: B";
        let mut a = engine(yaml);
        let mut b = engine(yaml);

        // Hold Ctrl on engine A only.
        assert_eq!(down(&mut a, HidUsage::LeftControl), Decision::Pass);

        // On A, Ctrl is held, so A fires the Ctrl+A rule.
        assert_eq!(
            down(&mut a, HidUsage::A),
            Decision::Emit {
                release: 1,
                outputs: vec![nk(HidUsage::B)]
            }
        );

        // On B, Ctrl is not held, so A is unmapped and passes through.
        assert_eq!(down(&mut b, HidUsage::A), Decision::Pass);
    }
}
