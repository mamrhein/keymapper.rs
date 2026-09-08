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

use std::{
    collections::{BTreeMap, BTreeSet},
    os::unix::io::{AsRawFd, RawFd},
    sync::Arc,
    thread,
    time::Duration,
};

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

/// Per-device key-fate tracking.
///
/// Kept separate from the evdev handle so the swallow/forward decision is
/// unit-testable.  The decisive fact is that a key-up is decided from its
/// key-down's own record rather than from a re-run of the lookup: the
/// modifier state may have changed in the meantime, which would leak the
/// release into the app as a phantom key-up, or swallow it while the
/// key-down passed through and leave the key held.
#[derive(Default)]
pub(super) struct KeyTracker {
    /// Bitmask of forwarded (unmapped) modifier keys that are still held on
    /// the virtual keyboard.
    forwarded_modifiers: u8,
    /// Bitmask of modifier keys that were part of a fired trigger and have
    /// already been released on the virtual keyboard.  Their physical release
    /// is swallowed so it is not forwarded a second time.
    consumed_modifiers: u8,
    /// Evdev codes of key-downs that fired a mapped trigger and were
    /// swallowed.  Their key-ups are swallowed unconditionally, regardless of
    /// the modifier state at release time.
    swallowed_keys: BTreeSet<u16>,
    /// Evdev codes of swallowed key-downs whose mapped output is a modifier
    /// key, with the mask of the modifier bits that are therefore held on
    /// the virtual keyboard.  The bits are released when the physical key is
    /// released, or when a later fired trigger consumes them.
    held_output_modifiers: BTreeMap<u16, u8>,
}

impl KeyTracker {
    /// Record a swallowed key-down.  Returns `true` only for the first press
    /// of a fresh key-down, not for auto-repeats (value 2), so a repeat is
    /// swallowed without re-firing the rule.
    fn record_swallowed_down(&mut self, code: u16, value: i32) -> bool {
        value == 1 && self.swallowed_keys.insert(code)
    }

    /// Decide the fate of a key-up.  Returns `Some(mask)` when the release is
    /// swallowed (its key-down fired a trigger, or the modifier was consumed
    /// by one), with *mask* the modifier bits held for that key's output that
    /// the caller must release on the virtual keyboard (0 when none).  Returns
    /// `None` when the release is forwarded; for a forwarded modifier the
    /// tracking bit is cleared.
    fn release(&mut self, code: u16, usage: HidUsage) -> Option<u8> {
        if self.swallowed_keys.remove(&code) {
            return Some(
                self.held_output_modifiers.remove(&code).unwrap_or(0)
            );
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

    /// Track a forwarded (unmapped) modifier press.  A fresh press clears any
    /// stale consumed mark, since the earlier release belonged to the previous
    /// press.
    fn record_forwarded_down(&mut self, bit: u8) {
        let mask = 1u8 << bit;
        self.forwarded_modifiers |= mask;
        self.consumed_modifiers &= !mask;
    }

    /// Consume the modifiers of a fired trigger and return
    /// `(released, held)`: *released* is the mask the caller must release on
    /// the virtual keyboard — the forwarded subset (moved into the consumed
    /// mask, so its physical release is swallowed) plus any held output
    /// modifier that was part of the trigger — and *held* is the subset of
    /// that mask that came from held outputs, so the caller can clear those
    /// bits from the lookup modifier state.
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

    /// Record that a swallowed key-down's mapped output holds modifier keys
    /// on the virtual keyboard.  The bits are released when the physical key
    /// is released, or when a later fired trigger consumes them.
    fn hold_output_modifiers(&mut self, code: u16, mask: u8) {
        if mask != 0 {
            self.held_output_modifiers
                .entry(code)
                .and_modify(|m| *m |= mask)
                .or_insert(mask);
        }
    }
}

/// A single managed keyboard device, tracking its own modifier state.
pub(super) struct ManagedDevice {
    pub(super) device: Device,
    /// Device node path (e.g. `/dev/input/event3`), used for rule lookup.
    pub(super) path: String,
    /// Bitmask of currently active modifiers for this device only.
    pub(super) modifiers: u8,
    /// Key-fate tracking (swallowed / forwarded / consumed).
    pub(super) tracking: KeyTracker,
    /// Last received `MSC_SCAN` value, consumed by the next `EV_KEY`
    /// event.  The kernel emits the scan code before the key event of the
    /// same press; key-ups and repeats carry no scan code, so those fall
    /// back to the `EV_KEY` reverse lookup.
    pub(super) pending_scan: Option<u32>,
    /// Set when a hot-plugged device was adopted before its current key state
    /// was synced to the virtual device.  The event loop syncs it once on the
    /// device's first pass, then clears this.  Devices grabbed at startup are
    /// synced inline in `start_mapping` and start `false`.
    pub(super) pending_initial_state: bool,
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

/// If the output key's base is itself a modifier key, return the mask of
/// modifier bits to hold on the virtual keyboard while the physical key is
/// pressed: the base's own bit plus any of the output's modifier bits.
/// Returns 0 for regular keys, which are emitted as taps.
fn output_held_mask(native_key: &NativeKey) -> u8 {
    let Some(bit) = HidUsage::hid_usage_to_modifier_bit(native_key.usage)
    else {
        return 0;
    };
    (1u8 << bit) | native_key.modifiers
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

        // Key-up: decide the fate from the key-down's own record rather than
        // a re-run of the lookup.  The modifier state may have changed since
        // the key-down (releasing a modifier is the common case), which would
        // otherwise leak the release into the app as a phantom key-up, or
        // swallow it while the key-down passed through and leave the key held.
        if value == 0 {
            match managed.tracking.release(code, usage) {
                Some(held) => {
                    // The key-down fired a trigger (or the modifier was
                    // consumed by one): swallow the release and, for a
                    // remapped modifier key, release the output bits that
                    // have been held since the key-down.
                    if held != 0 {
                        release_consumed_modifiers(virtual_device, held);
                        managed.modifiers &= !held;
                    }
                }
                None => forward_key_event(virtual_device, code, value),
            }
            continue;
        }

        // Key-down (value 1) or auto-repeat (value 2).
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
            // Record the key-down so its release is swallowed even if the
            // modifier state changes before the key-up.  Fire the rule —
            // release the trigger's held modifiers, then emit the outputs —
            // only on the first press; repeats are swallowed without
            // re-firing.
            if managed.tracking.record_swallowed_down(code, value) {
                // The trigger's modifiers were forwarded when pressed (or
                // are held by another remapped key's modifier output).
                // Release them now so the output is emitted as a clean tap;
                // forwarded marks are consumed so their physical release is
                // swallowed below, and held bits are cleared from the lookup
                // state since no physical key tracks them.
                let (consumed, held_consumed) =
                    managed.tracking.consume_triggered(lookup_modifiers);
                if consumed != 0 {
                    release_consumed_modifiers(virtual_device, consumed);
                }
                managed.modifiers &= !held_consumed;

                // If the physical key is itself a modifier, its bit was set
                // above for the pre-update lookup; it is mapped, not
                // forwarded, so clear it again.
                if let Some(bit) =
                    HidUsage::hid_usage_to_modifier_bit(usage)
                {
                    managed.modifiers &= !(1 << bit);
                }

                // Emit the outputs.  An output whose base is itself a
                // modifier key is held down on the virtual keyboard (not
                // tapped) so the remapped modifier stays active for
                // subsequent key presses; the matching release is emitted
                // when the physical key-up arrives.
                let mut held_mask: u8 = 0;
                for native_key in &outputs {
                    let mask = output_held_mask(native_key);
                    if mask != 0 {
                        match hold_modifier_output(virtual_device, native_key)
                        {
                            Ok(()) => {
                                managed
                                    .tracking
                                    .hold_output_modifiers(code, mask);
                                held_mask |= mask;
                            }
                            Err(e) => eprintln!("emit error: {}", e),
                        }
                    } else if let Err(e) =
                        emit_key_event(virtual_device, native_key)
                    {
                        eprintln!("emit error: {}", e);
                    }
                }
                if held_mask != 0 {
                    managed.modifiers |= held_mask;
                }
            }
            continue;
        }

        // Unmapped: track a forwarded modifier press so a later fired trigger
        // can release it cleanly.  A fresh press clears any stale consumed
        // mark, since the earlier release belonged to the previous press.
        if value == 1
            && let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage)
        {
            managed.tracking.record_forwarded_down(bit);
        }

        // Forward the event to the virtual device.
        forward_key_event(virtual_device, code, value);
    }
}

/// Emit the key-downs of a modifier-key output, keeping them held on the
/// virtual keyboard.
///
/// Unlike [`emit_key_event`] (which emits a self-contained tap), the keys
/// stay down until the physical key-up — which is what makes a remapped
/// modifier usable for the key presses that follow it.  On failure, any
/// keys that were pressed are released to prevent stuck state.
fn hold_modifier_output(
    device: &mut VirtualDevice,
    native_key: &NativeKey,
) -> Result<(), Box<dyn std::error::Error>> {
    // Raw evdev event type codes.
    const EV_KEY: u16 = 1;
    const EV_SYN: u16 = 0;
    const SYN_REPORT: u16 = 0;

    // The base modifier is pressed after the output's other modifier bits,
    // and its own bit is not emitted twice.
    let base_bit = HidUsage::hid_usage_to_modifier_bit(native_key.usage);
    let mut codes: Vec<u16> = Vec::new();
    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1
            && base_bit != Some(bit)
            && let Some(code) = modifier_bit_to_keycode(bit)
        {
            codes.push(code);
        }
    }
    if let Some(bit) = base_bit
        && let Some(code) = modifier_bit_to_keycode(bit)
    {
        codes.push(code);
    }

    // Track all pressed codes so they can be released on failure.
    let mut pressed: Vec<u16> = Vec::new();
    for code in codes {
        if let Err(e) = device.emit(&[
            InputEvent::new(EV_KEY, code, 1),
            InputEvent::new(EV_SYN, SYN_REPORT, 0),
        ]) {
            for pressed_code in pressed.iter().rev() {
                let _ = device.emit(&[
                    InputEvent::new(EV_KEY, *pressed_code, 0),
                    InputEvent::new(EV_SYN, SYN_REPORT, 0),
                ]);
                thread::sleep(EMIT_SPACING);
            }
            return Err(e.into());
        }
        pressed.push(code);
        thread::sleep(EMIT_SPACING);
    }

    Ok(())
}

/// Forward a raw evdev key event to the virtual device.
///
/// A repeat (value == 2) of a **modifier** is a no-op: its down is already
/// out, and emitting a press+release pair would release it mid-hold,
/// corrupting the modifier state for every key that follows it.  A repeat of
/// any other key is emitted as a press+release pair so each tick stays
/// visible to windowing backends that sample keyboard state once per frame.
fn forward_key_event(device: &mut VirtualDevice, code: u16, value: i32) {
    // Raw evdev event type codes.
    const EV_KEY: u16 = 1;
    const EV_SYN: u16 = 0;
    const SYN_REPORT: u16 = 0;

    if value == 2 {
        let is_modifier = keycode_to_hid_usage(code)
            .and_then(HidUsage::hid_usage_to_modifier_bit)
            .is_some();
        if is_modifier {
            return;
        }
        // Non-modifier repeat: emit as press+release to avoid key-stick.
        let events = [
            InputEvent::new(EV_KEY, code, 1),
            InputEvent::new(EV_KEY, code, 0),
            InputEvent::new(EV_SYN, SYN_REPORT, 0),
        ];
        if let Err(e) = device.emit(&events) {
            eprintln!("emit error: {e}");
        }
        return;
    }

    let events = [
        InputEvent::new(EV_KEY, code, value),
        InputEvent::new(EV_SYN, SYN_REPORT, 0),
    ];
    if let Err(e) = device.emit(&events) {
        eprintln!("emit error: {e}");
    }
}

/// Read the keyboard's current key state from the kernel and sync any held
/// keys to the virtual output device and this device's modifier tracking.
///
/// Grabbing a device delivers events only from the grab onward, so a key
/// (commonly a modifier) that is already held is invisible to the event
/// stream.  Without this sync the virtual keyboard and `managed.modifiers`
/// start out of step with the physical keyboard and stay that way, which is
/// why a modifier held at grab time silently breaks later key combinations.
pub(super) fn sync_initial_state(
    managed: &mut ManagedDevice,
    virtual_device: &mut VirtualDevice,
) {
    let Ok(held) = read_kernel_key_state(managed.device.as_raw_fd()) else {
        return;
    };
    for code in held {
        if let Some(usage) = keycode_to_hid_usage(code)
            && let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage)
        {
            // Restore the held modifier in the per-device lookup state and
            // mark it forwarded so its later physical release is forwarded
            // (not swallowed) to the virtual device.
            managed.modifiers |= 1 << bit;
            managed.tracking.record_forwarded_down(bit);
        }
        forward_key_event(virtual_device, code, 1);
    }
}

/// Build the `EVIOCGKEY` request word for a buffer of `buf_len` bytes.
///
/// `EVIOCGKEY = _IOC(_IOC_READ, 'E' (0x45), 0x18, buf_len)` using the generic
/// Linux ioctl layout: `nr` at bit 0, `type` at bit 8, `size` at bit 16,
/// and `dir` at bit 30.  The size field is 14 bits wide.
fn eviocgkey_request(buf_len: usize) -> libc::c_ulong {
    const IOC_READ: u64 = 2;
    const EVIOC_TYPE: u64 = 0x45; // 'E'
    const EVIOC_NR: u64 = 0x18;
    IOC_READ << 30
        | EVIOC_TYPE << 8
        | EVIOC_NR
        | ((buf_len as u64 & 0x3fff) << 16)
}

/// Read the set of currently-held key codes straight from the kernel via
/// `EVIOCGKEY`.  Returns the `KEY_*` codes the physical keyboard reports as
/// down at this instant.
fn read_kernel_key_state(fd: RawFd) -> std::io::Result<Vec<u16>> {
    const KEY_CNT: usize = 0x2fe; // KEY_MAX + 1
    let buf_len = KEY_CNT.div_ceil(8);
    let mut buf = vec![0u8; buf_len];
    let request = eviocgkey_request(buf_len);
    // Safety: `buf` is a valid writable buffer of exactly the length encoded
    // in the request word, and `fd` is the live evdev handle this device owns.
    let ret = unsafe { libc::ioctl(fd, request, buf.as_mut_ptr() as *mut _) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(parse_key_bitmap(&buf, KEY_CNT))
}

/// Decode a kernel `EVIOCGKEY` key bitmap (one bit per code, byte-major) into
/// the list of currently-held key codes, dropping any code beyond `key_cnt`.
fn parse_key_bitmap(buf: &[u8], key_cnt: usize) -> Vec<u16> {
    let mut held = Vec::new();
    for (i, &byte) in buf.iter().enumerate() {
        for bit in 0..8u8 {
            if byte & (1 << bit) != 0 {
                let code = (i * 8 + bit as usize) as u16;
                if code < key_cnt as u16 {
                    held.push(code);
                }
            }
        }
    }
    held
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
    // Initial-state bitmap parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_key_bitmap_sets_expected_codes() {
        // Bit 2 of byte 0 => code 2; bit 5 of byte 1 => code 13.
        let mut buf = [0u8; 2];
        buf[0] = 1 << 2;
        buf[1] = 1 << 5;
        assert_eq!(parse_key_bitmap(&buf, 100), vec![2u16, 13]);
    }

    #[test]
    fn parse_key_bitmap_ignores_codes_beyond_key_count() {
        // KEY_CNT = 5: codes 0..4 are valid; any higher bit must be dropped.
        // Byte 0 all ones = codes 0..7, of which only 0..4 are in range.
        let buf = [0xFFu8, 0u8];
        assert_eq!(parse_key_bitmap(&buf, 5), vec![0u16, 1, 2, 3, 4]);
    }

    #[test]
    fn parse_key_bitmap_empty_returns_nothing() {
        assert!(parse_key_bitmap(&[0u8; 4], 100).is_empty());
    }

    #[test]
    fn eviocgkey_request_matches_kernel_ioctl_layout() {
        // _IOC(_IOC_READ=2, 'E'=0x45, nr=0x18, size=96) =
        // (2 << 30) | (0x45 << 8) | (0x18 << 0) | (96 << 16).
        assert_eq!(eviocgkey_request(96), 0x80604518);
    }

    // -----------------------------------------------------------------------
    // Key-fate tracking tests
    // -----------------------------------------------------------------------
    //
    // Verifies that a key-up's fate is decided from the key-down's own record
    // (and the forwarded/consumed modifier state) rather than from a re-run of
    // the lookup, whose modifier state may have changed in the meantime.

    /// Evdev `KEY_LEFTCTRL`.
    const CTRL_CODE: u16 = 29;
    /// Evdev `KEY_A`.
    const A_CODE: u16 = 30;
    /// Evdev `KEY_CAPSLOCK`.
    const CAPS_CODE: u16 = 58;
    /// Evdev `KEY_F1`, used as a second physical key.
    const F1_CODE: u16 = 59;

    #[test]
    fn swallowed_key_down_swallows_its_key_up() {
        let mut t = KeyTracker::default();
        // The key-down fired a mapped trigger and was swallowed.
        assert!(t.record_swallowed_down(A_CODE, 1));
        // Its key-up is swallowed regardless of the modifier state, and the
        // record is consumed.
        assert_eq!(t.release(A_CODE, HidUsage::A), Some(0));
        assert_eq!(t.release(A_CODE, HidUsage::A), None);
    }

    #[test]
    fn mapped_base_release_swallowed_after_modifier_state_change() {
        // Models `Ctrl+Semicolon -> C` where the modifier is released before
        // the base.  The base's key-down fired the trigger (recorded); its
        // key-up arrives after the modifier state changed (Ctrl released), so
        // a re-derived lookup would not match and the release would leak as a
        // phantom key-up.  The record keeps it swallowed.
        let mut t = KeyTracker::default();

        // Ctrl down: forwarded (unmapped).
        t.record_forwarded_down(0);
        // Semicolon (base) down: fires the trigger.  Consume the held Ctrl and
        // record the base.
        assert_eq!(t.consume_triggered(1), (1, 0));
        assert!(t.record_swallowed_down(A_CODE, 1));

        // Ctrl up: consumed by the trigger, so swallowed.
        assert_eq!(t.release(CTRL_CODE, HidUsage::LeftControl), Some(0));

        // Base up: modifier state is now empty, but the base's key-down fired
        // a trigger, so its release is swallowed (not a phantom key-up).
        assert_eq!(t.release(A_CODE, HidUsage::A), Some(0));
    }

    #[test]
    fn forwarded_modifier_key_up_forwards_and_untracks() {
        let mut t = KeyTracker::default();
        t.record_forwarded_down(0);
        // Its release is forwarded and the tracking bit is cleared.
        assert_eq!(t.release(CTRL_CODE, HidUsage::LeftControl), None);
    }

    #[test]
    fn consumed_modifier_key_up_swallowed() {
        let mut t = KeyTracker::default();
        // Ctrl is forwarded, then consumed by a fired trigger.
        t.record_forwarded_down(0);
        assert_eq!(t.consume_triggered(1), (1, 0));
        // Its release is swallowed (already released on the virtual device).
        assert_eq!(t.release(CTRL_CODE, HidUsage::LeftControl), Some(0));
    }

    #[test]
    fn repeat_does_not_refire() {
        let mut t = KeyTracker::default();
        assert!(t.record_swallowed_down(A_CODE, 1)); // fresh press records
        assert!(!t.record_swallowed_down(A_CODE, 2)); // repeat does not re-record
    }

    #[test]
    fn fresh_forwarded_press_clears_stale_consumed_mark() {
        let mut t = KeyTracker::default();
        // Ctrl forwarded, then consumed by a trigger; its release is swallowed
        // by another path, leaving the consumed mark stale.
        t.record_forwarded_down(0);
        assert_eq!(t.consume_triggered(1), (1, 0));
        // A fresh Ctrl press must clear the stale mark so its release forwards
        // rather than being wrongly swallowed (which would leave it stuck).
        t.record_forwarded_down(0);
        assert_eq!(t.release(CTRL_CODE, HidUsage::LeftControl), None);
    }

    // -----------------------------------------------------------------------
    // Remapped-modifier (held output) tests
    // -----------------------------------------------------------------------
    //
    // Verifies that a rule whose output is a modifier key holds that
    // modifier on the virtual keyboard until the physical key-up, so the
    // remapped modifier stays active for subsequent key presses.

    #[test]
    fn modifier_output_is_held_until_physical_release() {
        // Models `CapsLock: LeftControl`: the key-down fires the trigger and
        // holds the output modifier on the virtual keyboard; the physical
        // key-up is swallowed and returns the held mask for release.
        let mut t = KeyTracker::default();
        assert!(t.record_swallowed_down(CAPS_CODE, 1));
        t.hold_output_modifiers(CAPS_CODE, 1); // LeftControl bit.

        assert_eq!(t.release(CAPS_CODE, HidUsage::CapsLock), Some(1));
        // A second release finds no record and is forwarded.
        assert_eq!(t.release(CAPS_CODE, HidUsage::CapsLock), None);
    }

    #[test]
    fn two_remapped_modifiers_held_independently() {
        // Two physical keys remapped to different modifiers: releasing one
        // must not affect the other.
        let mut t = KeyTracker::default();
        t.record_swallowed_down(CAPS_CODE, 1);
        t.hold_output_modifiers(CAPS_CODE, 1); // LeftControl
        t.record_swallowed_down(F1_CODE, 1);
        t.hold_output_modifiers(F1_CODE, 2); // LeftShift

        assert_eq!(t.release(CAPS_CODE, HidUsage::CapsLock), Some(1));
        assert_eq!(t.release(F1_CODE, HidUsage::F1), Some(2));
    }

    #[test]
    fn fired_trigger_consumes_held_output_modifier() {
        // Models `CapsLock: LeftControl` (held) followed by `Ctrl+Base: X`
        // fired while the remapped Ctrl is still held: the held bit is
        // released with the trigger's modifiers and removed from the map, so
        // the physical CapsLock key-up releases nothing.
        let mut t = KeyTracker::default();
        t.record_swallowed_down(CAPS_CODE, 1);
        t.hold_output_modifiers(CAPS_CODE, 1);

        assert_eq!(t.consume_triggered(1), (1, 1));
        assert_eq!(t.release(CAPS_CODE, HidUsage::CapsLock), Some(0));
    }

    #[test]
    fn consume_triggered_partial_held_mask_keeps_remaining_bits() {
        // A held output with two modifier bits consumed by a trigger that
        // carries only one of them: that bit is released and the other stays
        // held for the physical key-up.
        let mut t = KeyTracker::default();
        t.record_swallowed_down(CAPS_CODE, 1);
        t.hold_output_modifiers(CAPS_CODE, 3); // LeftControl | LeftShift

        assert_eq!(t.consume_triggered(1), (1, 1));
        assert_eq!(t.release(CAPS_CODE, HidUsage::CapsLock), Some(2));
    }

    #[test]
    fn output_held_mask_covers_modifier_bases() {
        // A regular key is a tap (no held bits); a modifier base holds its
        // own bit plus the output's modifier bits.
        assert_eq!(
            output_held_mask(&NativeKey {
                modifiers: 0,
                usage: HidUsage::A,
            }),
            0
        );
        assert_eq!(
            output_held_mask(&NativeKey {
                modifiers: 0,
                usage: HidUsage::LeftControl,
            }),
            1
        );
        assert_eq!(
            output_held_mask(&NativeKey {
                modifiers: 1,
                usage: HidUsage::LeftShift,
            }),
            3
        );
    }

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
