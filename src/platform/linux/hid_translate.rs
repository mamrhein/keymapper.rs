// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Translation between HID usages and evdev key codes.
//!
//! The kernel's `hid-input` driver translates HID reports into evdev events
//! inside the kernel, and that translation is not callable from user space.
//! The daemon therefore carries its own mapping:
//!
//! - `hid_usage_to_keycode` — output direction: converts a `HidUsage` to the
//!   evdev `KEY_*` code written to the uinput virtual keyboard.
//! - `keycode_to_hid_usage` — input fallback: converts an evdev `KEY_*` code
//!   back to a `HidUsage` for devices that do not emit `MSC_SCAN` (older
//!   kernels, some virtual devices).  For devices that emit `MSC_SCAN`, the
//!   scan code already encodes the full HID usage as `(page << 16) | id` and
//!   `HidUsage::from_code` resolves it without any table lookup.
//!
//! Both directions are derived from the single declarative table in
//! `common/hid_usage.rs`, which records each usage's evdev key code alongside
//! its HID code and config name, so no second hand-maintained list exists.
//! Key code values are derived from `include/uapi/linux/input-event-codes.h`.

use crate::common::hid_usage::HidUsage;

/// Map a HID usage to the evdev `KEY_*` code used for emission on the
/// uinput virtual keyboard.
///
/// Returns `None` for usages without a stable evdev equivalent.
pub fn hid_usage_to_keycode(usage: HidUsage) -> Option<u16> {
    usage.evdev_keycode()
}

/// Map an evdev `KEY_*` code to its HID usage.
///
/// Input fallback for devices that do not emit `MSC_SCAN` (older kernels,
/// some virtual devices).  Covers the keyboard page subset plus the media
/// and display key codes the kernel derives from Consumer Page usages, so
/// that physical media keys can still be matched against rules.
pub fn keycode_to_hid_usage(code: u16) -> Option<HidUsage> {
    HidUsage::ALL
        .iter()
        .copied()
        .find(|usage| usage.evdev_keycode() == Some(code))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_table_covers_all_known_usages() {
        // Every usage in HidUsage::ALL must have an evdev code so that no
        // compiled rule output can be dropped on emission.
        for &usage in HidUsage::ALL {
            assert!(
                hid_usage_to_keycode(usage).is_some(),
                "missing evdev code for {}",
                usage.as_str()
            );
        }
    }

    #[test]
    fn tables_round_trip() {
        // Forward and reverse lookups must be exact inverses.
        for &usage in HidUsage::ALL {
            let code = hid_usage_to_keycode(usage).expect("forward table");
            assert_eq!(
                keycode_to_hid_usage(code),
                Some(usage),
                "round-trip failed for {}",
                usage.as_str()
            );
        }
    }

    #[test]
    fn keyboard_page_values_match_kernel_header() {
        assert_eq!(
            hid_usage_to_keycode(HidUsage::A),
            Some(30) // KEY_A
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::Space),
            Some(57) // KEY_SPACE
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::LeftControl),
            Some(29) // KEY_LEFTCTRL
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::NumpadEnter),
            Some(96) // KEY_KPENTER
        );
    }

    #[test]
    fn consumer_page_values_match_kernel_header() {
        assert_eq!(
            hid_usage_to_keycode(HidUsage::PlayPause),
            Some(164) // KEY_PLAYPAUSE
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::VolumeUp),
            Some(115) // KEY_VOLUMEUP
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::VolumeDown),
            Some(114) // KEY_VOLUMEDOWN
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::Mute),
            Some(113) // KEY_MUTE
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::NextTrack),
            Some(163) // KEY_NEXTSONG
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::PreviousTrack),
            Some(165) // KEY_PREVIOUSSONG
        );
        assert_eq!(
            hid_usage_to_keycode(HidUsage::Stop),
            Some(166) // KEY_STOPCD
        );
    }

    #[test]
    fn reverse_table_resolves_media_key_codes() {
        assert_eq!(keycode_to_hid_usage(164), Some(HidUsage::PlayPause));
        assert_eq!(keycode_to_hid_usage(115), Some(HidUsage::VolumeUp));
        assert_eq!(keycode_to_hid_usage(114), Some(HidUsage::VolumeDown));
        assert_eq!(keycode_to_hid_usage(113), Some(HidUsage::Mute));
    }

    #[test]
    fn unknown_codes_return_none() {
        assert_eq!(keycode_to_hid_usage(0), None);
        assert_eq!(keycode_to_hid_usage(0x2FF), None);
    }
}
