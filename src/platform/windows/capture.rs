// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows capture: shared hook-event decode and read-only observe
//! mode.
//!
//! Both the daemon's low-level hook path (`mapping`) and the read-only
//! observe mode consumed by `keymapper keys probe`
//! ([`WindowsBackend::observe`](super::backend::WindowsBackend)) feed
//! their raw hook events through [`KeyScanner`], so the VK-to-usage
//! decode and the direction classification have exactly one
//! implementation and the two paths cannot drift.

use std::error::Error;

use crossbeam_channel::{Sender, unbounded};
use windows::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, KBDLLHOOKSTRUCT,
        MSG, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
        WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
    },
};

use super::key::Key;
use crate::{common::keyboard::KeyboardInfo, platform::backend::CapturedKey};

/// Turns raw low-level keyboard hook events into [`CapturedKey`]s.
///
/// A `WH_KEYBOARD_LL` event is self-contained — the virtual-key code
/// and the press direction are both in the callback's parameters — so
/// this scanner owns no cross-event buffering (contrast Linux's
/// `MSC_SCAN` pairing): every hook event yields exactly one captured
/// key, with `usage: None` for virtual-key codes the [`Key`] table
/// cannot resolve (callers decide what an unknown key means).
/// [`Direction::Repeat`](crate::keymap_core::logfmt::Direction) is
/// never produced: the low-level hook delivers an auto-repeat as a
/// plain key-down, so a repeat cannot be distinguished from a fresh
/// press here.
pub(crate) struct KeyScanner;

impl KeyScanner {
    pub(crate) const fn new() -> Self {
        Self
    }

    /// Feed one raw hook event: the callback's virtual-key code and
    /// whether it was a key-down (`WM_KEYDOWN`/`WM_SYSKEYDOWN`).
    pub(crate) fn on_event(
        &self,
        vk_code: u16,
        is_key_down: bool,
    ) -> CapturedKey {
        CapturedKey {
            native: vk_code,
            value: i32::from(is_key_down),
            usage: Key::from_native(vk_code).map(Key::to_hid_usage),
        }
    }
}

/// The scanner shared by the daemon's hook proc and the observe hook
/// proc.  [`KeyScanner`] is stateless, so this single instance needs
/// no synchronization; the low-level hook chain is serialized by the
/// OS.
pub(crate) static SCANNER: KeyScanner = KeyScanner::new();

// ---------------------------------------------------------------------------
// Observe mode (the passive pump behind `KeySource::observe`)
// ---------------------------------------------------------------------------

/// Sink the observe hook proc feeds its decoded events into.
///
/// A Windows hook proc carries no refcon (unlike a macOS CGEventTap),
/// so the sender lives in a static for the duration of the pump:
/// [`observe`] installs it, the hook proc clones it per event, and
/// [`observe`] retires it when the pump ends.
static OBSERVE_TX: parking_lot::Mutex<Option<Sender<CapturedKey>>> =
    parking_lot::Mutex::new(None);

/// Observe `keyboard` in read-only mode and pump every decoded key
/// event through `on_key`.
///
/// This is the counterpart of Linux's un-grabbed device open and
/// macOS' `ListenOnly` tap: a `WH_KEYBOARD_LL` hook proc that decodes
/// and always calls `CallNextHookEx` — it can neither delay nor
/// swallow an event.  The pump runs until the process terminates
/// (Ctrl+C; the OS removes the hook with the process); the `Err` path
/// reports only a failure to install the hook.
///
/// `keyboard` selects nothing: the low-level hook is session-global
/// and `KBDLLHOOKSTRUCT` carries no originating device (F7a), the
/// same shape as macOS' tap.  Unlike the daemon's hook, the observe
/// proc deliberately does *not* filter [`INJECTED_TAG`](super::INJECTED_TAG)
/// events: that filter is the daemon's own echo-suppression mechanism,
/// and a read-only pump must see every event the hook sees.
pub(crate) fn observe(
    _keyboard: &KeyboardInfo,
    on_key: &mut dyn FnMut(CapturedKey),
) -> Result<(), Box<dyn Error>> {
    let (tx, rx) = unbounded();
    *OBSERVE_TX.lock() = Some(tx);

    let h_instance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();
    let handle: HHOOK = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(observe_keyboard_proc),
            Some(h_instance),
            0,
        )?
    };
    if handle.is_invalid() {
        return Err("failed to install the keyboard hook".into());
    }

    // A `WH_KEYBOARD_LL` callback only runs while the installing
    // thread pumps messages, so the hook and the drain share this
    // loop: callbacks fire during the wait's message processing and
    // every event they sent is drained once the wait returns.  The
    // loop leaves only on `WM_QUIT` or a queue error (nothing posts
    // `WM_QUIT`; Ctrl+C terminates the process), which is the pump
    // running until the process ends, per the contract.
    unsafe {
        let mut msg = MSG::default();
        loop {
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                break;
            }
            // TranslateMessage's return value is not used; the message
            // is always dispatched regardless of whether it was
            // translated.
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            while let Ok(key) = rx.try_recv() {
                on_key(key);
            }
        }
    }
    // The pump has ended; retire the sink before unhooking.
    *OBSERVE_TX.lock() = None;
    unsafe { UnhookWindowsHookEx(handle)? };
    Ok(())
}

/// The observe hook proc: decode, hand the event to the pump through
/// the channel, and always let it pass.
///
/// The passive half of observe mode: it never swallows, and the
/// channel send is non-blocking (unbounded), so the input chain is
/// never delayed beyond the callback itself.
extern "system" fn observe_keyboard_proc(
    code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if code >= 0 {
        let kbd_struct = unsafe { &*(l_param.0 as *const KBDLLHOOKSTRUCT) };
        let is_key_down =
            matches!(w_param.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let key = SCANNER.on_event(kbd_struct.vkCode as u16, is_key_down);
        if let Some(tx) = OBSERVE_TX.lock().clone() {
            let _ = tx.send(key);
        }
    }
    unsafe { CallNextHookEx(None, code, w_param, l_param) }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{common::hid_usage::HidUsage, keymap_core::logfmt::Direction};

    /// VK_A
    const VK_A: u16 = 0x41;
    /// VK_LSHIFT
    const VK_LSHIFT: u16 = 0xA0;
    /// An undefined virtual-key code with no `Key` variant.
    const VK_UNDEFINED: u16 = 0x07;

    #[test]
    fn key_down_decodes_to_press_with_usage() {
        let scanner = KeyScanner::new();
        let captured = scanner.on_event(VK_A, true);
        assert_eq!(captured.native, VK_A);
        assert_eq!(captured.value, 1);
        assert_eq!(captured.direction(), Direction::Down);
        assert_eq!(captured.usage, Some(HidUsage::A));
    }

    #[test]
    fn key_up_decodes_to_release() {
        let scanner = KeyScanner::new();
        let captured = scanner.on_event(VK_A, false);
        assert_eq!(captured.value, 0);
        assert_eq!(captured.direction(), Direction::Up);
        assert_eq!(captured.usage, Some(HidUsage::A));
    }

    #[test]
    fn modifier_key_decodes_to_its_usage() {
        let scanner = KeyScanner::new();
        let captured = scanner.on_event(VK_LSHIFT, true);
        assert_eq!(captured.usage, Some(HidUsage::LeftShift));
    }

    #[test]
    fn unresolvable_vk_captures_without_usage() {
        let scanner = KeyScanner::new();
        let captured = scanner.on_event(VK_UNDEFINED, true);
        assert_eq!(captured.native, VK_UNDEFINED);
        assert_eq!(captured.usage, None);
    }

    #[test]
    fn every_mapped_vk_resolves_to_a_usage() {
        // Every `Key` variant's VK round-trips through the scanner to
        // the variant's own usage (Keyboard page for ordinary keys,
        // Consumer page for the media variants).
        let scanner = KeyScanner::new();
        for key in Key::ALL {
            let captured = scanner.on_event(key.as_native(), true);
            assert_eq!(
                captured.usage,
                Some(key.to_hid_usage()),
                "VK {:#04X} should resolve to {:?}",
                key.as_native(),
                key.to_hid_usage(),
            );
        }
    }
}
