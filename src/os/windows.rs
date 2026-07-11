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
use windows_sys::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW,
        UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
        WM_SYSKEYDOWN, WM_SYSKEYUP,
    },
};

use crate::{RuntimeState, mapping_cache::NativeAction};

static mut HOOK_HANDLE: windows_sys::Win32::UI::WindowsAndFiltering::HHOOK = 0;
static mut SHARED_STATE: Option<Arc<RwLock<RuntimeState>>> = None;

pub(crate) fn start_mapping(
    state: Arc<RwLock<RuntimeState>>,
) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        SHARED_STATE = Some(state);
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
        if let Some(ref state) = SHARED_STATE {
            let kbd_struct = *(l_param as *const KBDLLHOOKSTRUCT);
            let vk_code = kbd_struct.vkCode;

            let is_key_down = w_param as u32 == WM_KEYDOWN
                || w_param as u32 == WM_SYSKEYDOWN;
            let is_key_up =
                w_param as u32 == WM_KEYUP || w_param as u32 == WM_SYSKEYUP;

            let state_guard = state.read();
            let current_app = state_guard.active_app.to_lowercase();

            let mut active_action = state_guard
                .lookup_cache
                .process_map
                .get(&current_app)
                .and_then(|m| m.get(&vk_code));
            if active_action.is_none() {
                active_action =
                    state_guard.lookup_cache.global_map.get(&vk_code);
            }

            if let Some(action) = active_action {
                match action {
                    NativeAction::RemapTo(target_vk) => {
                        simulate_key_event(*target_vk as u8, is_key_up);
                    }
                    NativeAction::Shortcut(target_vks) => {
                        if is_key_down {
                            for vk in target_vks {
                                simulate_key_event(*vk as u8, false);
                            }
                        } else if is_key_up {
                            for vk in target_vks.iter().rev() {
                                simulate_key_event(*vk as u8, true);
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

unsafe fn simulate_key_event(vk_byte: u8, is_key_up: bool) {
    use windows_sys::Win32::UI::WindowsAndFiltering::{
        KEYEVENTF_KEYUP, keybd_event,
    };
    let flags = if is_key_up { KEYEVENTF_KEYUP } else { 0 };
    keybd_event(vk_byte, 0, flags, 0);
}
