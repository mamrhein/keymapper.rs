// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows implementation of `keymapper keys probe`.

use std::sync::OnceLock;

use keymapperd::os::Key;
use windows_sys::{
    Windows::Win32::{
        Foundation::HWND,
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{MapVirtualKeyW, VIRTUAL_KEY},
            WindowsAndMessaging::{
                CallNextHookEx, DispatchMessageW, GetKeyState, GetMessageW,
                HHOOK, HINSTANCE, KBDLLHOOKSTRUCT, LPARAM, LRESULT, MSG,
                PostQuitMessage, SetWindowsHookExW, TranslateMessage,
                UnhookWindowsHookEx, VK_CONTROL, WH_KEYBOARD_LL, WM_KEYDOWN,
                WM_SYSKEYDOWN, WPARAM,
            },
        },
    },
    core::PCWSTR,
};

/// Windows virtual-key code for the Control key.
const VK_LCONTROL: VIRTUAL_KEY = 0xA2;
const VK_RCONTROL: VIRTUAL_KEY = 0xA3;

static HOOK_HANDLE: OnceLock<HHOOK> = OnceLock::new();

/// Check whether a virtual-key code corresponds to a modifier key.
fn is_modifier(vk: VIRTUAL_KEY) -> bool {
    matches!(
        vk,
        VK_LCONTROL
            | VK_RCONTROL
            | 0xA0 // VK_LSHIFT
            | 0xA1 // VK_RSHIFT
            | 0xA4 // VK_LMENU (LeftAlt)
            | 0xA5 // VK_RMENU (RightAlt)
            | 0x5B // VK_LWIN (LeftCommand)
            | 0x5C // VK_RWIN (RightCommand)
    )
}

/// Probe for key presses using a WH_KEYBOARD_LL hook.
pub fn probe() {
    println!("Press keys to see their names and codes.");
    println!("Press Control+Escape to exit.\n");

    let h_instance: HINSTANCE =
        unsafe { GetModuleHandleW(PCWSTR(std::ptr::null())) };

    let handle: HHOOK = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(probe_keyboard_proc),
            h_instance,
            0,
        )
    };

    if handle == 0 {
        eprintln!("Failed to install keyboard hook");
        std::process::exit(1);
    }

    HOOK_HANDLE.set(handle).ok();

    // Run the message loop.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, HWND(0), 0, 0) > 0 {
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
        return unsafe { CallNextHookEx(0, code, w_param, l_param) };
    }

    let kbd_struct = unsafe { *(l_param as *const KBDLLHOOKSTRUCT) };
    let vk_code = kbd_struct.vkCode as VIRTUAL_KEY;

    let is_key_down =
        w_param as u32 == WM_KEYDOWN || w_param as u32 == WM_SYSKEYDOWN;

    // Check for Ctrl+Escape exit condition.
    if is_key_down && vk_code == 0x1B {
        // VK_ESCAPE
        let ctrl_state = unsafe { GetKeyState(VK_CONTROL) };
        if ctrl_state < 0 {
            unsafe { PostQuitMessage(0) };
            return unsafe { CallNextHookEx(0, code, w_param, l_param) };
        }
    }

    // Skip modifier keys.
    if is_modifier(vk_code) {
        return unsafe { CallNextHookEx(0, code, w_param, l_param) };
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

    unsafe { CallNextHookEx(0, code, w_param, l_param) }
}
