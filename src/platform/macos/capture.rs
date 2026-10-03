// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Shared CGEvent capture and keycode decode.
//!
//! Both the daemon's active CGEventTap (`mapping::tap_callback`) and the
//! read-only observe mode consumed by `keymapper keys probe`
//! ([`MacOsBackend::observe`](super::backend::MacOsBackend)) feed their
//! events through [`KeyScanner`], so the keycode-to-usage decode and the
//! direction classification — including the CapsLock toggle quirk —
//! have exactly one implementation and the two paths cannot drift.
//!
//! Unlike Linux, there is no per-device file handle to open: a
//! CGEventTap is session-global and sees every keyboard, so observe
//! mode runs a *passive* tap (`CGEventTapOptions::ListenOnly`) that
//! shares the event stream with the system instead of grabbing a
//! device, and the [`KeyboardInfo`] selects nothing (CGEvents expose no
//! originating device — see F7a).  This is also why the tap callback
//! stays in `mapping` while the decode step lives here.

use std::{error::Error, ffi::c_void, ptr::NonNull};

use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventSource,
    CGEventSourceStateID, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType,
};

use super::keycode::keycode_to_hid_usage;
use crate::{
    common::{hid_usage::HidUsage, keyboard::KeyboardInfo},
    platform::backend::CapturedKey,
};

// ---------------------------------------------------------------------------
// Scanning (the shared decode step)
// ---------------------------------------------------------------------------

/// Turns raw CGEvents into [`CapturedKey`]s.
///
/// Key-down and key-up events decode trivially from their event type.
/// Modifier presses and releases arrive as `FlagsChanged` events with
/// no key-down/key-up counterpart: their direction is read from the
/// event's flag mask, except for CapsLock — a toggle key whose flag
/// stays set after the physical release — which the scanner classifies
/// from the change against its own previous observation (see
/// [`Self::capslock_is_down`]).
pub(crate) struct KeyScanner {
    /// The alpha-shift (caps lock) flag state as of the last CapsLock
    /// event, or at scanner creation when there was none yet.  Touched
    /// only on the thread pumping the tap's run loop, so no
    /// synchronization is needed.
    prev_alpha_shift: bool,
}

impl KeyScanner {
    /// Create a scanner seeded with the current alpha-shift state, so
    /// the first CapsLock event is classified against reality rather
    /// than an assumption.
    pub(crate) fn new() -> Self {
        Self {
            prev_alpha_shift: CGEventSource::flags_state(
                CGEventSourceStateID::HIDSystemState,
            )
            .contains(CGEventFlags::MaskAlphaShift),
        }
    }

    /// Decode one CGEvent.  Returns a [`CapturedKey`] for keyboard
    /// events whose direction can be classified, `None` for everything
    /// else (mouse events, `Fn`, and other non-modifier
    /// `FlagsChanged`s whose direction no flag reveals).  The usage is
    /// `None` for keycodes the translation table cannot resolve —
    /// callers decide what an unknown key means.
    pub(crate) fn on_event(
        &mut self,
        event_type: CGEventType,
        event: &CGEvent,
    ) -> Option<CapturedKey> {
        let native = CGEvent::integer_value_field(
            Some(event),
            CGEventField::KeyboardEventKeycode,
        ) as u16;
        match event_type {
            CGEventType::KeyDown => Some(CapturedKey {
                native,
                value: 1,
                usage: keycode_to_hid_usage(native),
            }),
            CGEventType::KeyUp => Some(CapturedKey {
                native,
                value: 0,
                usage: keycode_to_hid_usage(native),
            }),
            CGEventType::FlagsChanged => {
                let usage = keycode_to_hid_usage(native)?;
                let flags = CGEvent::flags(Some(event));
                let is_down = if usage == HidUsage::CapsLock {
                    self.capslock_is_down(flags)
                } else {
                    self.flags_changed_state(usage, flags)?
                };
                Some(CapturedKey {
                    native,
                    value: i32::from(is_down),
                    usage: Some(usage),
                })
            }
            _ => None,
        }
    }

    /// Compute the down/up state of a `FlagsChanged` event for one of
    /// the eight held modifiers, from its usage and flag mask.
    ///
    /// The state is read from the modifier's flag: set means down,
    /// cleared means up.  Returns `None` for usages that are not held
    /// modifiers (CapsLock is a toggle key and is handled separately by
    /// [`Self::capslock_is_down`], as are Fn, etc.).
    fn flags_changed_state(
        &self,
        usage: HidUsage,
        flags: CGEventFlags,
    ) -> Option<bool> {
        HidUsage::hid_usage_to_modifier_bit(usage).map(|bit| match bit {
            0 | 4 => flags.contains(CGEventFlags::MaskControl),
            1 | 5 => flags.contains(CGEventFlags::MaskShift),
            2 | 6 => flags.contains(CGEventFlags::MaskAlternate),
            _ => flags.contains(CGEventFlags::MaskCommand), // 3 | 7
        })
    }

    /// Classify a CapsLock `FlagsChanged` event as a press or a
    /// release.
    ///
    /// CapsLock is a toggle key with no modifier bit: the alpha-shift
    /// flag stays set after the physical release, so the event's own
    /// mask cannot say whether it is a press or a release (a release
    /// with caps on looks exactly like a press with caps off).  Only
    /// the change between consecutive events can: a press toggles the
    /// state, a release leaves it unchanged.
    fn capslock_is_down(&mut self, flags: CGEventFlags) -> bool {
        let alpha_shift = flags.contains(CGEventFlags::MaskAlphaShift);
        let is_down = alpha_shift != self.prev_alpha_shift;
        self.prev_alpha_shift = alpha_shift;
        is_down
    }
}

// ---------------------------------------------------------------------------
// Observe mode (the passive pump behind `KeySource::observe`)
// ---------------------------------------------------------------------------

/// State shared with the observe tap callback via its refcon.
struct ObserveContext<'a> {
    scanner: KeyScanner,
    on_key: &'a mut dyn FnMut(CapturedKey),
}

/// Observe `keyboard` in read-only mode and pump every decoded key
/// event through `on_key`.
///
/// The tap is created with [`CGEventTapOptions::ListenOnly`]: it shares
/// the event stream with the system and can neither delay nor swallow
/// an event — the passive counterpart of Linux observe mode, where the
/// device is opened but never grabbed.  The pump polls the current
/// CFRunLoop until the process terminates, so the probe ends with
/// Ctrl+C (SIGINT terminates the process; macOS removes the tap with
/// it).  The `Err` path reports only a failure to create the tap or
/// schedule its port.
///
/// `keyboard` selects nothing: a CGEventTap is session-global and sees
/// every keyboard (CGEvents expose no originating device, F7a).
pub(crate) fn observe(
    _keyboard: &KeyboardInfo,
    on_key: &mut dyn FnMut(CapturedKey),
) -> Result<(), Box<dyn Error>> {
    // Observe key-down, key-up, and modifier (flags-changed) events at
    // the earliest tap point.  `CGEventMask` is a bitmask whose bit N
    // selects event type N, so each type must be shifted into its own
    // bit (a plain OR of the raw type values would select the
    // low-numbered mouse events; see `mapping::start_mapping`).
    let mask: CGEventMask = (1u64 << CGEventType::KeyDown.0)
        | (1u64 << CGEventType::KeyUp.0)
        | (1u64 << CGEventType::FlagsChanged.0);

    let mut ctx = ObserveContext {
        scanner: KeyScanner::new(),
        on_key,
    };
    let refcon = &mut ctx as *mut ObserveContext<'_> as *mut c_void;

    let tap_port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            mask,
            Some(observe_callback),
            refcon,
        )
    }
    .ok_or(
        "failed to create the CGEventTap. Grant `keymapper` Input Monitoring \
         and Accessibility access in System Settings → Privacy & Security, \
         then run it again.",
    )?;

    // Schedule the tap's mach port on the current run loop so its
    // callbacks fire.
    let source = CFMachPort::new_run_loop_source(None, Some(&tap_port), 0)
        .ok_or("failed to create the tap run-loop source")?;
    CFRunLoop::current()
        .ok_or("failed to get the current run loop")?
        .add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });

    // A CGEventTap is created disabled; enable it now that its port is
    // scheduled, so callbacks fire as soon as the run loop starts.
    CGEvent::tap_enable(&tap_port, true);

    // Poll the run loop until the process terminates.
    // `kCFRunLoopDefaultMode` is a member of the common modes set and
    // receives the tap's mach-port callbacks.
    loop {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.5, true);
    }
}

/// The observe tap callback: decode, hand upward, and let the event
/// pass.  A listen-only tap cannot modify or swallow events, so the
/// original is always returned.
unsafe extern "C-unwind" fn observe_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    refcon: *mut c_void,
) -> *mut CGEvent {
    // A listen-only tap never blocks, so the system does not disable
    // it; if it ever is (secure input), keep passing events through.
    if event_type == CGEventType::TapDisabledByTimeout
        || event_type == CGEventType::TapDisabledByUserInput
    {
        return event.as_ptr();
    }

    let ctx = unsafe { &mut *(refcon as *mut ObserveContext<'_>) };
    if let Some(key) =
        ctx.scanner.on_event(event_type, unsafe { event.as_ref() })
    {
        (ctx.on_key)(key);
    }
    event.as_ptr()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CFRetained;

    use super::*;
    use crate::{
        common::hid_usage::PAGE_KEYBOARD, keymap_core::logfmt::Direction,
    };

    /// The CGKeyCode of the left Shift key.
    const KC_LEFT_SHIFT: u16 = 56;
    /// The CGKeyCode of the 'A' key.
    const KC_A: u16 = 0;
    /// The CGKeyCode of CapsLock.
    const KC_CAPSLOCK: u16 = 57;
    /// The CGKeyCode of the `Fn` key.
    const KC_FN: u16 = 63;

    /// A synthetic keyboard CGEvent carrying a keycode and a flag mask.
    /// The keycode must be set at creation: `CGEventSetIntegerValueField`
    /// silently fails on the null-type event that `CGEventCreate` returns,
    /// leaving the keycode at 0.  The event is never posted to the HID
    /// event system, which the scanner tests never do.
    fn event(keycode: u16, flags: CGEventFlags) -> CFRetained<CGEvent> {
        let event = CGEvent::new_keyboard_event(None, keycode, false)
            .expect("CGEvent::new_keyboard_event");
        CGEvent::set_flags(Some(&event), flags);
        event
    }

    #[test]
    fn key_down_decodes_to_press_with_usage() {
        let mut scanner = KeyScanner::new();
        let captured = scanner
            .on_event(
                CGEventType::KeyDown,
                &event(KC_A, CGEventFlags::empty()),
            )
            .expect("key-down decodes");
        assert_eq!(captured.native, KC_A);
        assert_eq!(captured.value, 1);
        assert_eq!(captured.direction(), Direction::Down);
        assert_eq!(captured.usage, keycode_to_hid_usage(KC_A));
    }

    #[test]
    fn key_up_decodes_to_release() {
        let mut scanner = KeyScanner::new();
        let captured = scanner
            .on_event(CGEventType::KeyUp, &event(KC_A, CGEventFlags::empty()))
            .expect("key-up decodes");
        assert_eq!(captured.value, 0);
        assert_eq!(captured.direction(), Direction::Up);
    }

    /// A held modifier's state is read from its flag: set means down,
    /// cleared means up.  Right-side modifiers share the flag with their
    /// left-side twin.
    #[test]
    fn flags_changed_reads_direction_from_the_flag_mask() {
        let mut scanner = KeyScanner::new();
        // Left Shift down: the shift flag is set on the event.
        let down = scanner
            .on_event(
                CGEventType::FlagsChanged,
                &event(KC_LEFT_SHIFT, CGEventFlags::MaskShift),
            )
            .expect("shift decodes");
        assert_eq!(down.direction(), Direction::Down);
        assert_eq!(down.usage, Some(HidUsage::LeftShift));
        // Left Shift up: same keycode, flag now clear.
        let up = scanner
            .on_event(
                CGEventType::FlagsChanged,
                &event(KC_LEFT_SHIFT, CGEventFlags::empty()),
            )
            .expect("shift decodes");
        assert_eq!(up.direction(), Direction::Up);
    }

    /// A usage that is neither a held modifier nor CapsLock has no flag
    /// that reveals a direction: the scanner reports nothing (the same
    /// events the daemon passes through untouched).
    #[test]
    fn non_modifier_usage_is_not_classifiable() {
        let mut scanner = KeyScanner::new();
        assert!(
            scanner
                .on_event(
                    CGEventType::FlagsChanged,
                    &event(KC_A, CGEventFlags::empty())
                )
                .is_none()
        );
    }

    /// CapsLock is a toggle key: the alpha-shift flag stays set after
    /// the physical release, so only the change between consecutive
    /// events distinguishes a press from a release.  A state change is
    /// a press; an unchanged state is a release, regardless of the
    /// flag's value.
    #[test]
    fn capslock_press_release_from_state_change() {
        // Starting with caps off: the press sets the flag (down), the
        // release leaves it set (up).
        let mut scanner = KeyScanner {
            prev_alpha_shift: false,
        };
        let down = scanner
            .on_event(
                CGEventType::FlagsChanged,
                &event(KC_CAPSLOCK, CGEventFlags::MaskAlphaShift),
            )
            .expect("capslock decodes");
        assert_eq!(down.direction(), Direction::Down);
        assert_eq!(down.usage, Some(HidUsage::CapsLock));
        let up = scanner
            .on_event(
                CGEventType::FlagsChanged,
                &event(KC_CAPSLOCK, CGEventFlags::MaskAlphaShift),
            )
            .expect("capslock decodes");
        assert_eq!(up.direction(), Direction::Up);

        // Starting with caps on: the first press clears the flag
        // (down), and the release leaves it cleared (up).
        let mut scanner = KeyScanner {
            prev_alpha_shift: true,
        };
        let down = scanner
            .on_event(
                CGEventType::FlagsChanged,
                &event(KC_CAPSLOCK, CGEventFlags::empty()),
            )
            .expect("capslock decodes");
        assert_eq!(down.direction(), Direction::Down);
        let up = scanner
            .on_event(
                CGEventType::FlagsChanged,
                &event(KC_CAPSLOCK, CGEventFlags::empty()),
            )
            .expect("capslock decodes");
        assert_eq!(up.direction(), Direction::Up);
    }

    #[test]
    fn fn_flags_changed_is_not_classifiable() {
        let mut scanner = KeyScanner::new();
        // `Fn` has no held-modifier flag: the scanner cannot classify
        // its direction and reports nothing (the same events the daemon
        // used to pass through silently).
        assert!(
            scanner
                .on_event(
                    CGEventType::FlagsChanged,
                    &event(KC_FN, CGEventFlags::empty())
                )
                .is_none()
        );
    }

    #[test]
    fn non_keyboard_event_produces_no_captured_key() {
        let mut scanner = KeyScanner::new();
        assert!(
            scanner
                .on_event(
                    CGEventType::LeftMouseDown,
                    &event(KC_A, CGEventFlags::empty())
                )
                .is_none()
        );
    }

    /// Keycodes the translation table cannot resolve still capture,
    /// with a `None` usage for the caller to interpret (the probe
    /// prints `Unknown(code)`).
    #[test]
    fn unresolvable_keycode_captures_without_usage() {
        let mut scanner = KeyScanner::new();
        let captured = scanner
            .on_event(
                CGEventType::KeyDown,
                &event(u16::MAX, CGEventFlags::empty()),
            )
            .expect("unknown code still captures");
        assert_eq!(captured.usage, None);
        assert_eq!(captured.native, u16::MAX);
    }

    /// The macOS keycode table maps only to keyboard-page usages.
    /// Pin that invariant, since `flags_changed_state`'s modifier-bit
    /// mapping (and the Karabiner IPC encoding) assume it.
    #[test]
    fn every_mapped_keycode_resolves_to_a_keyboard_page_usage() {
        for kc in 0..=0x00FFu16 {
            if let Some(usage) = keycode_to_hid_usage(kc) {
                assert_eq!(
                    usage.page(),
                    PAGE_KEYBOARD,
                    "CGKeyCode {kc} maps to non-keyboard page {usage:?}",
                );
            }
        }
    }
}
