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
//! [`ManagedDevice`] wraps a single grabbed keyboard's evdev handle and its
//! [`MappingEngine`], which owns the modifier state and key-fate tracking that
//! is kept independently for each physical device.
//! [`process_device_events`] drains the device's pending input, resolves each
//! key's HID identity, asks the engine for a decision, and either emits the
//! mapped outputs or forwards the raw event to the virtual output device.

use std::{
    os::unix::io::{AsRawFd, RawFd},
    thread,
    time::Duration,
};

use evdev::{Device, EventType, InputEvent, MiscCode, uinput::VirtualDevice};

use crate::{
    common::{hid_usage::HidUsage, modifier::ModifierRole},
    daemon::{
        engine::{Decision, MappingEngine, output_held_mask},
        mapping_cache::NativeKey,
    },
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
    /// The mapping engine: owns this device's modifier state and key-fate
    /// tracking (swallowed / forwarded / consumed).
    pub(super) engine: MappingEngine<u16>,
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

        // The engine decides the event's fate from its own bookkeeping
        // (pressed/swallowed keys, forwarded/consumed modifier masks, held
        // output modifiers); this layer only executes the decision.  A repeat
        // (value 2) is a key-down for this purpose: the engine deduplicates
        // it against its pressed set.
        match managed.engine.decide(
            code,
            usage,
            value != 0,
            Some(&managed.path),
            true,
        ) {
            // Unmapped (or the repeat of an unmapped key): forward the raw
            // event to the virtual device.
            Decision::Pass => forward_key_event(virtual_device, code, value),
            // Mapped: release the trigger's modifiers first (clean tap),
            // then emit the outputs.  An output whose base is itself a
            // modifier key is held down on the virtual keyboard (not tapped)
            // so the remapped modifier stays active for subsequent key
            // presses; the matching release is emitted when the physical
            // key-up arrives.
            Decision::Emit { release, outputs } => {
                if release != 0 {
                    release_consumed_modifiers(virtual_device, release);
                }
                for native_key in &outputs {
                    if output_held_mask(native_key).is_some() {
                        if let Err(e) =
                            hold_modifier_output(virtual_device, native_key)
                        {
                            eprintln!("emit error: {}", e);
                        }
                    } else if let Err(e) =
                        emit_key_event(virtual_device, native_key)
                    {
                        eprintln!("emit error: {}", e);
                    }
                }
            }
            // A mapped key-up (or a consumed modifier release): swallow the
            // event and, for a remapped modifier key, release the output bits
            // that have been held since the key-down.
            Decision::Swallow { release } => {
                if release != 0 {
                    release_consumed_modifiers(virtual_device, release);
                }
            }
        }
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
/// keys to the virtual output device and this device's engine.
///
/// Grabbing a device delivers events only from the grab onward, so a key
/// (commonly a modifier) that is already held is invisible to the event
/// stream.  Without this sync the virtual keyboard and the engine's modifier
/// state start out of step with the physical keyboard and stay that way,
/// which is why a modifier held at grab time silently breaks later key
/// combinations.
pub(super) fn sync_initial_state(
    managed: &mut ManagedDevice,
    virtual_device: &mut VirtualDevice,
) {
    let Ok(held) = read_kernel_key_state(managed.device.as_raw_fd()) else {
        return;
    };
    for code in held {
        // Note the held key in the engine so its later release (and any
        // repeats) are not treated as a fresh press; for a modifier this
        // also restores the bit in the lookup state and marks it forwarded
        // so its release is forwarded (not swallowed) to the virtual device.
        if let Some(usage) = keycode_to_hid_usage(code) {
            managed.engine.note_held_key(code, usage);
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
    // Modifier keycode mapping tests
    // -----------------------------------------------------------------------
    //
    // Verifies that the modifier-bit-to-evdev-code path agrees with the
    // shared HID usage table.

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
}
