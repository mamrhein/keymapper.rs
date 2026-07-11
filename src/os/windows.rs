// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    ptr::null_mut,
    sync::{Arc, OnceLock},
};

use parking_lot::RwLock;
use windows_sys::Windows::Win32::{
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
            SendInput, VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, HHOOK, HINSTANCE, KBDLLHOOKSTRUCT,
            LPARAM, LRESULT, MSG, SetWindowsHookExW, UnhookWindowsHookEx,
            WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
            WPARAM,
        },
    },
};

use crate::{mapping_cache::NativeAction, state::Lookup};

/// Safe, single-assignment globals that replace the former `static mut`.
/// `OnceLock` guarantees: set exactly once, then immutable shared reads.
/// Internal mutation (cache hot-swap, active-app updates) is handled by
/// the `RwLock` inside the `Arc`, not by unsafe aliasing.
static SHARED_LOOKUP: OnceLock<Arc<RwLock<dyn Lookup>>> = OnceLock::new();
static HOOK_HANDLE: OnceLock<HHOOK> = OnceLock::new();

/// Initialise the shared lookup table.  Panics if called more than once
/// (should never happen in normal flow).
fn set_shared_lookup(lookup: Arc<RwLock<dyn Lookup>>) {
    SHARED_LOOKUP
        .set(lookup)
        .expect("shared lookup already initialised");
}

/// Initialise the hook handle.  Panics if called more than once.
fn set_hook_handle(handle: HHOOK) {
    HOOK_HANDLE
        .set(handle)
        .expect("hook handle already initialised");
}

/// Get the stored hook handle.  Safe because `OnceLock` provides
/// immutable shared access after initialisation.
fn hook_handle() -> HHOOK {
    *HOOK_HANDLE
        .get()
        .expect("hook handle not initialised — call start_mapping first")
}

pub(crate) fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Populate the safe global before the hook can fire.
    set_shared_lookup(lookup);

    let h_instance: HINSTANCE = unsafe { GetModuleHandleW(null_mut()) };

    let handle: HHOOK = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(low_level_keyboard_proc),
            h_instance,
            0,
        )
    };

    if handle == 0 {
        return Err("Failed to install global keyboard hook".into());
    }
    set_hook_handle(handle);
    println!("Windows low-level hook listening...");

    // Message loop — blocks until a WM_QUIT message is posted.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {}
        UnhookWindowsHookEx(hook_handle());
    }

    Ok(())
}

extern "system" fn low_level_keyboard_proc(
    code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if code < 0 {
        return unsafe {
            CallNextHookEx(hook_handle(), code, w_param, l_param)
        };
    }

    let Some(lookup) = SHARED_LOOKUP.get() else {
        return unsafe {
            CallNextHookEx(hook_handle(), code, w_param, l_param)
        };
    };

    let kbd_struct = unsafe { *(l_param as *const KBDLLHOOKSTRUCT) };
    // vkCode (u32) narrows to VIRTUAL_KEY (u16) — safe for all
    // defined VK_* constants (max 0xFF).
    let vk_code = kbd_struct.vkCode as VIRTUAL_KEY;

    let is_key_down =
        w_param as u32 == WM_KEYDOWN || w_param as u32 == WM_SYSKEYDOWN;
    let is_key_up =
        w_param as u32 == WM_KEYUP || w_param as u32 == WM_SYSKEYUP;

    let guard = lookup.read();
    let current_app = guard.active_app().to_lowercase();
    let active_action = guard
        .for_app(&current_app, vk_code)
        .or_else(|| guard.global(vk_code));

    if let Some(action) = active_action {
        match action {
            NativeAction::RemapTo(target_vk) => {
                simulate_key_event(*target_vk, is_key_up);
            }
            NativeAction::Shortcut(target_vks) => {
                if is_key_down {
                    for vk in target_vks.iter() {
                        simulate_key_event(*vk, false);
                    }
                } else if is_key_up {
                    for vk in target_vks.iter().rev() {
                        simulate_key_event(*vk, true);
                    }
                }
            }
        }
        return 1; // Swallow the original key
    }

    unsafe { CallNextHookEx(hook_handle(), code, w_param, l_param) }
}

/// Inject a synthetic key event via `SendInput` (modern replacement for
/// the deprecated `keybd_event`).  `vk` is `VIRTUAL_KEY` (u16) — matching
/// both `NativeKey` and the API natively.
fn simulate_key_event(vk: VIRTUAL_KEY, is_key_up: bool) {
    let mut input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if is_key_up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(
            1,
            std::ptr::addr_of!(input),
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}
