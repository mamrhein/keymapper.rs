// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows implementation of `keymapper keys probe`.

use windows_sys::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{
            GetKeyState, MapVirtualKeyW, VIRTUAL_KEY, VK_CONTROL,
        },
        WindowsAndMessaging::{
            CallNextHookEx, DispatchMessageW, GetMessageW, KBDLLHOOKSTRUCT,
            MSG, PostQuitMessage, SetWindowsHookExW, TranslateMessage,
            UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
        },
    },
};

/// Type aliases for hook types not re-exported in windows-sys 0.61.
#[allow(clippy::upper_case_acronyms)]
type HHOOK = *mut std::ffi::c_void;

use crate::platform::Key;

/// Windows virtual-key code for the Control key.
const VK_LCONTROL: VIRTUAL_KEY = 0xA2;
const VK_RCONTROL: VIRTUAL_KEY = 0xA3;

static HOOK_HANDLE: parking_lot::Mutex<isize> = parking_lot::Mutex::new(0);

/// Probe for key presses using a WH_KEYBOARD_LL hook.
pub fn probe() {
    println!("Press keys to see their names and codes.");
    println!("Press Control+Escape to exit.\n");

    let h_instance: HINSTANCE =
        unsafe { GetModuleHandleW(std::ptr::null::<u16>()) };

    let handle: HHOOK = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(probe_keyboard_proc),
            h_instance,
            0,
        )
    };

    if handle.is_null() {
        eprintln!("Failed to install keyboard hook");
        std::process::exit(1);
    }

    *HOOK_HANDLE.lock() = handle as isize;

    // Run the message loop.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, 0 as _, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UnhookWindowsHookEx(handle);
    }
}

/// Hook callback for key probing.
extern "system" fn probe_keyboard_proc(
    code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if code < 0 {
        return unsafe {
            CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param)
        };
    }

    let kbd_struct = unsafe { *(l_param as *const KBDLLHOOKSTRUCT) };
    let vk_code = kbd_struct.vkCode as VIRTUAL_KEY;

    let is_key_down =
        w_param as u32 == WM_KEYDOWN || w_param as u32 == WM_SYSKEYDOWN;

    // Check for Ctrl+Escape exit condition.
    if is_key_down && vk_code == 0x1B {
        // VK_ESCAPE
        let ctrl_state = unsafe { GetKeyState(VK_CONTROL as i32) };
        if ctrl_state < 0 {
            unsafe { PostQuitMessage(0) };
            return unsafe {
                CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param)
            };
        }
    }

    // Print on key down.
    if is_key_down {
        let (name, code_str) = if let Some(key) =
            Key::from_native(vk_code as u16)
        {
            (key.as_str().to_string(), format!("{}", key.as_native()))
        } else {
            // Try to get a character representation.
            let char_code = unsafe { MapVirtualKeyW(vk_code as u32, 2) };
            let name = if char_code != 0 && (char_code as u8) as char != '\0' {
                format!("Unknown({}, {})", vk_code, char_code as u8 as char)
            } else {
                format!("Unknown({vk_code})")
            };
            (name, format!("{vk_code}"))
        };

        println!("{name}: {code_str}");
    }

    unsafe { CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param) }
}
