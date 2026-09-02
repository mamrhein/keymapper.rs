// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Per-managed-device state and input event processing.
//!
//! [`ManagedDevice`] wraps a single grabbed keyboard's evdev handle and the
//! modifier state that is tracked independently for each physical device.
//! [`process_device_events`] drains the device's pending input, resolves each
//! key's HID identity, applies the active rules, and either emits the mapped
//! outputs or forwards the raw event to the virtual output device.

use std::{sync::Arc, thread, time::Duration};

use evdev::{Device, EventType, InputEvent, MiscCode, uinput::VirtualDevice};
use parking_lot::RwLock;

use crate::{
    common::{hid_usage::HidUsage, modifier::ModifierRole},
    daemon::{mapping_cache::NativeKey, state::Lookup},
    platform::linux::hid_translate::{
        hid_usage_to_keycode, keycode_to_hid_usage,
    },
};

// ---------------------------------------------------------------------------
// Per-device state
// ---------------------------------------------------------------------------

/// A single managed keyboard device, tracking its own modifier state.
pub(super) struct ManagedDevice {
    pub(super) device: Device,
    /// Device node path (e.g. `/dev/input/event3`), used for rule lookup.
    pub(super) path: String,
    /// Bitmask of currently active modifiers for this device only.
    pub(super) modifiers: u8,
    /// Bitmask of forwarded (unmapped) modifier keys that are still held.
    /// Mapped modifiers are excluded so their self-contained output taps
    /// do not leak into later state.
    pub(super) forwarded_modifiers: u8,
    /// Bitmask of modifier keys that were part of a fired trigger and have
    /// already been released on the virtual keyboard.  Their physical
    /// release is swallowed so it is not forwarded a second time.
    pub(super) consumed_modifiers: u8,
    /// Last received `MSC_SCAN` value, consumed by the next `EV_KEY`
    /// event.  The kernel emits the scan code before the key event of the
    /// same press; key-ups and repeats carry no scan code, so those fall
    /// back to the `EV_KEY` reverse lookup.
    pub(super) pending_scan: Option<u32>,
}

// ---------------------------------------------------------------------------
// Modifier handling
// ---------------------------------------------------------------------------

/// Map a modifier bit position to the evdev `KEY_*` code for emission.
///
/// The bit resolves to a `ModifierRole` (the canonical layout lives in
/// `common::modifier`); the resulting modifier usage is looked up in the
/// shared `hid_translate` table like any other key.
fn modifier_bit_to_keycode(bit: u8) -> Option<u16> {
    let role = ModifierRole::try_from_bit(bit)?;
    let usage = HidUsage::keyboard(role.hid_id())?;
    hid_usage_to_keycode(usage)
}

/// Spacing between sub-events of a chord emission.
///
/// Windowing backends and e2e monitor windows sample keyboard state once
/// per frame (typically 16-30 ms).  A tap that fits entirely between two
/// samples is invisible to them, so each sub-event is held long enough to
/// guarantee at least one sample inside the press window.
const EMIT_SPACING: Duration = Duration::from_millis(20);

/// Emit a complete key event (press+release) through the virtual device.
///
/// Handles chord emission: modifiers are pressed, the base key is toggled,
/// then modifiers are released in reverse order. On failure, any keys that
/// were pressed are released to prevent stuck state.
fn emit_key_event(
    device: &mut VirtualDevice,
    native_key: &NativeKey,
) -> Result<(), Box<dyn std::error::Error>> {
    // Raw evdev event type codes.
    const EV_KEY: u16 = 1;
    const EV_SYN: u16 = 0;
    const SYN_REPORT: u16 = 0;

    // Resolve the output's base key to an evdev `KEY_*` code via the
    // static HID translation table.
    let Some(base_code) = hid_usage_to_keycode(native_key.usage) else {
        return Err(format!(
            "no evdev key code for HID usage {:?}",
            native_key.usage
        )
        .into());
    };

    // Track all pressed codes so they can be released on failure.
    let mut pressed: Vec<u16> = Vec::new();

    // Helper to emit a single event with synchronization.
    let emit = |dev: &mut VirtualDevice,
                code: u16,
                val: i32|
     -> Result<(), Box<dyn std::error::Error>> {
        dev.emit(&[
            InputEvent::new(EV_KEY, code, val),
            InputEvent::new(EV_SYN, SYN_REPORT, 0),
        ])?;
        Ok(())
    };

    // Helper to release any keys that were successfully pressed.
    let cleanup = |dev: &mut VirtualDevice, codes: &[u16]| {
        for code in codes.iter().rev() {
            let _ = emit(dev, *code, 0);
            thread::sleep(EMIT_SPACING);
        }
    };

    // Press modifiers.
    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1
            && let Some(code) = modifier_bit_to_keycode(bit)
        {
            emit(device, code, 1)?;
            pressed.push(code);
            thread::sleep(EMIT_SPACING);
        }
    }

    // Press and release the base key.
    emit(device, base_code, 1)?;
    thread::sleep(Duration::from_millis(1));
    emit(device, base_code, 0)?;
    thread::sleep(Duration::from_millis(1));

    // Release modifiers in reverse order.
    cleanup(device, &pressed);

    Ok(())
}

// ---------------------------------------------------------------------------
// Per-device event processing
// ---------------------------------------------------------------------------

/// Process all pending events for a single managed device.
///
/// Uses the device's own modifier state and path for rule lookup, ensuring
/// that modifier state on one keyboard does not affect another.
pub(super) fn process_device_events(
    managed: &mut ManagedDevice,
    virtual_device: &mut VirtualDevice,
    lookup: &Arc<RwLock<dyn Lookup>>,
) {
    // Drain all pending events from this non-blocking device.
    let events = match managed.device.fetch_events() {
        Ok(events) => events,
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            return;
        }
        Err(e) => {
            eprintln!(
                "Linux: error reading events from {}: {}",
                managed.path, e
            );
            return;
        }
    };

    for event in events {
        // MSC_SCAN events carry the raw HID usage as
        // `(page << 16) | id`, and the kernel emits them before the
        // EV_KEY event of the same key press.  Buffer the scan code for
        // the next key event.
        if event.event_type() == EventType::MISC
            && event.code() == MiscCode::MSC_SCAN.0
        {
            managed.pending_scan = Some(event.value() as u32);
            continue;
        }

        if event.event_type() != EventType::KEY {
            continue;
        }

        let code = event.code();
        let value = event.value();

        // Derive the HID identity of this key.  MSC_SCAN is preferred;
        // the EV_KEY reverse lookup covers key-ups, auto-repeats, and
        // devices that do not emit MSC_SCAN.
        let usage = managed
            .pending_scan
            .take()
            .and_then(HidUsage::from_code)
            .or_else(|| keycode_to_hid_usage(code));

        let Some(usage) = usage else {
            // Unknown key with no resolvable HID identity: forward it
            // unchanged.
            forward_key_event(virtual_device, code, value);
            continue;
        };

        // Capture the modifier state to use for rule matching.  For modifier
        // keys this is the pre-update snapshot so that bare-modifier triggers
        // (e.g. "LeftControl: A") match correctly against the concurrent
        // modifier set.
        let lookup_modifiers = managed.modifiers;

        if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage) {
            if value == 1 {
                managed.modifiers |= 1 << bit;
            } else if value == 0 {
                managed.modifiers &= !(1 << bit);
            }
        }

        let device_path = &managed.path;

        // Compiled rules store the trigger as a `HidUsage`, so the
        // lookup is keyed by the full page-specific usage.
        let guard = lookup.read();
        let active_outputs = guard
            .for_active_app(usage, lookup_modifiers, Some(device_path))
            .or_else(|| {
                guard.global(usage, lookup_modifiers, Some(device_path))
            })
            .map(|v| v.to_vec());
        drop(guard);

        if let Some(outputs) = active_outputs {
            // Emit mapped outputs and swallow the original event.  This
            // applies to modifier keys as well: if a bare modifier
            // (e.g. LeftControl alone) is mapped, its outputs are emitted
            // and the original modifier press is NOT forwarded to the
            // virtual device, preventing double emission.
            if value == 1 {
                // The trigger's modifiers were forwarded when pressed.
                // Release them now so the output is emitted as a clean tap;
                // mark them consumed so their physical release is swallowed
                // below rather than forwarded a second time.
                let consumed = lookup_modifiers & managed.forwarded_modifiers;
                if consumed != 0 {
                    managed.forwarded_modifiers &= !consumed;
                    managed.consumed_modifiers |= consumed;
                    release_consumed_modifiers(virtual_device, consumed);
                }

                for native_key in &outputs {
                    if let Err(e) = emit_key_event(virtual_device, native_key)
                    {
                        eprintln!("emit error: {}", e);
                    }
                }
            }
            continue;
        }

        // A modifier key-up that did not fire a trigger: a consumed modifier
        // (already released when its trigger fired) and a mapped
        // bare-modifier trigger's release are both swallowed, while a
        // forwarded modifier's release is forwarded and untracked.
        if value == 0
            && let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage)
        {
            let mask = 1u8 << bit;
            if managed.consumed_modifiers & mask != 0 {
                managed.consumed_modifiers &= !mask;
                continue;
            }
            if managed.forwarded_modifiers & mask == 0 {
                // The press was mapped (a bare-modifier trigger) and never
                // forwarded, so its release is swallowed.
                continue;
            }
            managed.forwarded_modifiers &= !mask;
        }

        // Track a forwarded (unmapped) modifier press so a later fired
        // trigger can release it cleanly.
        if value == 1
            && let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage)
        {
            managed.forwarded_modifiers |= 1 << bit;
        }

        // Forward the event to the virtual device.
        forward_key_event(virtual_device, code, value);
    }
}

/// Forward a raw evdev key event to the virtual device.
///
/// Repeat events (value == 2) are emitted as a press+release pair to
/// avoid key-stick on the virtual device.
fn forward_key_event(device: &mut VirtualDevice, code: u16, value: i32) {
    // Raw evdev event type codes.
    const EV_KEY: u16 = 1;
    const EV_SYN: u16 = 0;
    const SYN_REPORT: u16 = 0;

    let events = if value == 2 {
        // Repeat event: emit as press+release to avoid key-stick.
        vec![
            InputEvent::new(EV_KEY, code, 1),
            InputEvent::new(EV_KEY, code, 0),
            InputEvent::new(EV_SYN, SYN_REPORT, 0),
        ]
    } else {
        vec![
            InputEvent::new(EV_KEY, code, value),
            InputEvent::new(EV_SYN, SYN_REPORT, 0),
        ]
    };

    if let Err(e) = device.emit(&events) {
        eprintln!("emit error: {e}");
    }
}

/// Release a set of consumed trigger modifiers on the virtual device.
///
/// Emits a key-up for each set bit in *consumed* (ascending bit order) so
/// the fired trigger's modifiers are dropped before the mapped output is
/// emitted.  Without this the output would ride on the still-held modifier
/// and produce an unintended control sequence (e.g. the rule
/// `Ctrl+Semicolon -> C` would emit Ctrl+C, i.e. SIGINT).
fn release_consumed_modifiers(device: &mut VirtualDevice, consumed: u8) {
    // Raw evdev event type codes.
    const EV_KEY: u16 = 1;
    const EV_SYN: u16 = 0;
    const SYN_REPORT: u16 = 0;

    for bit in 0..8 {
        if consumed & (1 << bit) != 0
            && let Some(code) = modifier_bit_to_keycode(bit)
        {
            let _ = device.emit(&[
                InputEvent::new(EV_KEY, code, 0),
                InputEvent::new(EV_SYN, SYN_REPORT, 0),
            ]);
            thread::sleep(EMIT_SPACING);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Per-device modifier isolation tests
    // -----------------------------------------------------------------------
    //
    // Verifies that modifier state is tracked independently per device.
    // Ctrl pressed on device A must not affect the modifier bitmask of
    // device B.

    #[test]
    fn modifier_bit_to_keycode_maps_all_modifiers() {
        // All eight modifier bits resolve to the corresponding evdev
        // codes; bit 8 is out of range.
        assert_eq!(modifier_bit_to_keycode(0), Some(29)); // KEY_LEFTCTRL
        assert_eq!(modifier_bit_to_keycode(1), Some(42)); // KEY_LEFTSHIFT
        assert_eq!(modifier_bit_to_keycode(2), Some(56)); // KEY_LEFTALT
        assert_eq!(modifier_bit_to_keycode(3), Some(125)); // KEY_LEFTMETA
        assert_eq!(modifier_bit_to_keycode(4), Some(97)); // KEY_RIGHTCTRL
        assert_eq!(modifier_bit_to_keycode(5), Some(54)); // KEY_RIGHTSHIFT
        assert_eq!(modifier_bit_to_keycode(6), Some(100)); // KEY_RIGHTALT
        assert_eq!(modifier_bit_to_keycode(7), Some(126)); // KEY_RIGHTMETA
        assert_eq!(modifier_bit_to_keycode(8), None);
    }

    #[test]
    fn modifier_bit_matches_hid_usage_table() {
        // The bit->keycode path must agree with the shared HID usage
        // table for all eight modifier usages.
        for usage in [
            HidUsage::LeftControl,
            HidUsage::RightControl,
            HidUsage::LeftShift,
            HidUsage::RightShift,
            HidUsage::LeftAlt,
            HidUsage::RightAlt,
            HidUsage::LeftCommand,
            HidUsage::RightCommand,
        ] {
            let bit = HidUsage::hid_usage_to_modifier_bit(usage)
                .expect("modifier usage");
            assert_eq!(
                modifier_bit_to_keycode(bit),
                hid_usage_to_keycode(usage),
                "modifier round-trip failed for {usage:?}"
            );
        }
    }

    #[test]
    fn modifier_state_is_isolated_per_device() {
        // Simulate two independent devices by tracking their own modifier
        // bitmasks, mirroring the logic in `process_device_events`.
        let mut mods_a: u8 = 0;
        let mut mods_b: u8 = 0;

        // Device A: press LeftControl (bit 0).
        let bit = HidUsage::hid_usage_to_modifier_bit(HidUsage::LeftControl)
            .unwrap();
        mods_a |= 1 << bit;
        assert_eq!(mods_a, 0b0000_0001);
        assert_eq!(mods_b, 0); // Device B unaffected.

        // Device A: press LeftShift (bit 1).
        let bit =
            HidUsage::hid_usage_to_modifier_bit(HidUsage::LeftShift).unwrap();
        mods_a |= 1 << bit;
        assert_eq!(mods_a, 0b0000_0011);
        assert_eq!(mods_b, 0); // Device B unaffected.

        // Device B: press RightAlt (bit 6).
        let bit =
            HidUsage::hid_usage_to_modifier_bit(HidUsage::RightAlt).unwrap();
        mods_b |= 1 << bit;
        assert_eq!(mods_a, 0b0000_0011); // Device A unaffected.
        assert_eq!(mods_b, 0b0100_0000);

        // Device A: release LeftControl.
        let bit = HidUsage::hid_usage_to_modifier_bit(HidUsage::LeftControl)
            .unwrap();
        mods_a &= !(1 << bit);
        assert_eq!(mods_a, 0b0000_0010);
        assert_eq!(mods_b, 0b0100_0000);

        // Device B: release RightAlt, press LeftCommand (bit 3).
        let bit =
            HidUsage::hid_usage_to_modifier_bit(HidUsage::RightAlt).unwrap();
        mods_b &= !(1 << bit);
        let bit = HidUsage::hid_usage_to_modifier_bit(HidUsage::LeftCommand)
            .unwrap();
        mods_b |= 1 << bit;
        assert_eq!(mods_a, 0b0000_0010);
        assert_eq!(mods_b, 0b0000_1000);
    }
}
