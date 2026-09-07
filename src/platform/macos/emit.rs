// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Virtual HID keyboard emission for mapped outputs.
//!
//! This is the virtkbdd side of the two-process design: it turns a single
//! mapped-output [`NativeKey`] into a sequence of DriverKit virtual-keyboard
//! reports.  It is the simplified successor of the legacy `emit_hid_report` in
//! the IOKit seizure capture path.  Because the virtual keyboard is now used
//! only for mapped outputs (unmapped keys pass through natively), there is no
//! forwarded key state to preserve across reports.

use super::karabiner_client::KarabinerClient;
use crate::{
    common::hid_usage::PAGE_KEYBOARD, daemon::mapping_cache::NativeKey,
};

/// Emit a single mapped-output key through the Karabiner virtual keyboard.
///
/// Emits the output as a sequence of state-snapshot reports so that every key
/// transition is its own report: each output modifier down (ascending bit
/// order), the base key down, the base key up, then each output modifier up
/// (descending bit order).  Posting one transition per report makes the
/// captured event order deterministic — a single report carrying both a
/// modifier and the base key would let IOKit deliver the two values in an
/// unspecified order.
///
/// Dispatches on the output usage's page:
/// - Keyboard page (0x07): a 67-byte `keyboard_input` report per transition.
/// - Consumer page (0x0C): a `consumer_input` report, then an all-clear.
pub fn emit_native_key(conn: &KarabinerClient, native_key: &NativeKey) {
    if native_key.usage.page() == PAGE_KEYBOARD {
        for (modifiers, usages) in keyboard_report_sequence(native_key) {
            let _ = conn.send_keyboard_report(modifiers, &usages);
        }
    } else {
        // Consumer page: post the usage, then an all-clear report to release.
        let _ = conn.send_consumer_report(native_key.usage.id());
        let _ = conn.send_consumer_release();
    }
}

/// Compute the sequence of keyboard reports that emit a single keyboard-page
/// [`NativeKey`].
///
/// Returns one `(modifiers, usages)` pair per report, in emission order: each
/// output modifier down (ascending bit order), the base key down, the base key
/// up, then each output modifier up (descending bit order).  Splitting this
/// out from [`emit_native_key`] makes the report sequence directly testable
/// without a live virtual keyboard.
fn keyboard_report_sequence(native_key: &NativeKey) -> Vec<(u8, Vec<u16>)> {
    let base_usage = native_key.usage.id();
    let output_modifiers = native_key.modifiers;

    let mut reports: Vec<(u8, Vec<u16>)> = Vec::new();
    let mut modifiers = 0u8;

    // Press each output modifier, one at a time in ascending bit order.
    for bit in 0..8 {
        if (output_modifiers >> bit) & 1 == 1 {
            modifiers |= 1 << bit;
            reports.push((modifiers, Vec::new()));
        }
    }

    // Press the base key with all output modifiers held, then release it.
    reports.push((modifiers, vec![base_usage]));
    reports.push((modifiers, Vec::new()));

    // Release each output modifier, one at a time in descending bit order.
    for bit in (0..8).rev() {
        if (output_modifiers >> bit) & 1 == 1 {
            modifiers &= !(1 << bit);
            reports.push((modifiers, Vec::new()));
        }
    }

    reports
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::hid_usage::HidUsage;

    /// A single modifier plus a base key produces the canonical four-report
    /// sequence: modifier down, base down, base up, modifier up.
    #[test]
    fn sequence_modifier_and_key() {
        let native_key = NativeKey {
            modifiers: 0x02, // left shift
            usage: HidUsage::E,
        };
        let base = HidUsage::E.id();
        assert_eq!(
            keyboard_report_sequence(&native_key),
            vec![
                (0x02, Vec::new()), // left shift down
                (0x02, vec![base]), // base key down with shift held
                (0x02, Vec::new()), // base key up
                (0x00, Vec::new()), // left shift up
            ]
        );
    }

    /// A bare key with no modifiers is just a down/up pair.
    #[test]
    fn sequence_no_modifiers() {
        let native_key = NativeKey {
            modifiers: 0,
            usage: HidUsage::A,
        };
        let base = HidUsage::A.id();
        assert_eq!(
            keyboard_report_sequence(&native_key),
            vec![(0, vec![base]), (0, Vec::new())]
        );
    }

    /// Multiple modifiers press in ascending bit order and release in
    /// descending bit order, so the captured event order is deterministic.
    #[test]
    fn sequence_multiple_modifiers_ascending_descending() {
        let native_key = NativeKey {
            modifiers: 0x03, // left control (bit 0) + left shift (bit 1)
            usage: HidUsage::A,
        };
        let base = HidUsage::A.id();
        assert_eq!(
            keyboard_report_sequence(&native_key),
            vec![
                (0x01, Vec::new()), // left control down
                (0x03, Vec::new()), // left shift down
                (0x03, vec![base]), // base key down
                (0x03, Vec::new()), // base key up
                // Releasing left shift (bit 1) while left control (bit 0) is
                // still held leaves modifiers = 0x01, then 0x00.
                (0x01, Vec::new()), // left shift up (descending)
                (0x00, Vec::new()), // left control up
            ]
        );
    }
}
