// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{ptr::null_mut, sync::Arc};

use parking_lot::RwLock;
use windows_sys::Windows::Win32::{
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
            SendInput, VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, HINSTANCE, KBDLLHOOKSTRUCT, LPARAM,
            LRESULT, MSG, SetWindowsHookExW, UnhookWindowsHookEx,
            WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
            WPARAM,
        },
    },
};

use crate::{mapping_cache::NativeAction, state::Lookup};

static mut HOOK_HANDLE:
    windows_sys::Windows::Win32::UI::WindowsAndFiltering::HHOOK = 0;
// TODO(#8): Replace static mut with a safe alternative (e.g., thread-local
// or hook-proc redesign). Windows LL keyboard hook API does not support
// passing user data, so a global is currently unavoidable.
static mut SHARED_LOOKUP: Option<Arc<RwLock<dyn Lookup>>> = None;

pub(crate) fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        SHARED_LOOKUP = Some(lookup);
        let h_instance: HINSTANCE = GetModuleHandleW(null_mut());

        HOOK_HANDLE = SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(low_level_keyboard_proc),
            h_instance,
            0,
        );

        if HOOK_HANDLE == 0 {
            return Err("Failed to install global hook".into());
        }
        println!("Windows low-level hook listening...");

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {}

        UnhookWindowsHookEx(HOOK_HANDLE);
    }
    Ok(())
}

unsafe extern "system" fn low_level_keyboard_proc(
    code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if code >= 0 {
        if let Some(ref lookup) = SHARED_LOOKUP {
            let kbd_struct = *(l_param as *const KBDLLHOOKSTRUCT);
            // vkCode is u32 in the hook struct, but VK codes fit in u16
            // (VIRTUAL_KEY).  Narrowing is safe for all defined VK_*
            // constants.
            let vk_code = kbd_struct.vkCode as VIRTUAL_KEY;

            let is_key_down = w_param as u32 == WM_KEYDOWN
                || w_param as u32 == WM_SYSKEYDOWN;
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
                return 1; // Swallow key
            }
        }
    }
    CallNextHookEx(HOOK_HANDLE, code, w_param, l_param)
}

/// Inject a synthetic key event via `SendInput` (modern replacement for
/// deprecated `keybd_event`).  `Vk` is `u16` — matching both `NativeKey`
/// and `VIRTUAL_KEY`.
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
