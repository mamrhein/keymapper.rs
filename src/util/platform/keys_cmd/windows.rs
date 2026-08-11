// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows implementation of `keymapper keys probe`.

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, MapVirtualKeyW, VIRTUAL_KEY, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, KBDLLHOOKSTRUCT,
    MSG, PostQuitMessage, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use crate::platform::Key;

// `HHOOK` wraps a raw `*mut c_void` which is not `Send`.  We store it as
// a raw pointer in a usize, which is `Send` and `Sync`.  This is safe
// because the hook handle is only ever read/written through the mutex.
type RawHookHandle = usize;

static HOOK_HANDLE: parking_lot::Mutex<RawHookHandle> =
    parking_lot::Mutex::new(0);

/// Probe for key presses using a WH_KEYBOARD_LL hook.
pub fn probe() {
    let h_instance: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .expect("Failed to get module handle")
        .into();

    let handle: HHOOK = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(probe_keyboard_proc),
            h_instance,
            0,
        )
        .expect("Failed to set keyboard hook")
    };

    if handle.is_invalid() {
        eprintln!("Failed to install keyboard hook");
        std::process::exit(1);
    }

    *HOOK_HANDLE.lock() = handle.0 as RawHookHandle;

    println!("Press keys to see their names and codes.");
    println!("Press Control+Escape to exit.\n");

    // Run the message loop.
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UnhookWindowsHookEx(handle).expect("Failed to unhook keyboard hook");
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
            CallNextHookEx(HHOOK::default(), code, w_param, l_param)
        };
    }

    let kbd_struct = unsafe { &*(l_param.0 as *const KBDLLHOOKSTRUCT) };
    let vk_code = VIRTUAL_KEY(kbd_struct.vkCode as u16);

    let is_key_down =
        w_param.0 as u32 == WM_KEYDOWN || w_param.0 as u32 == WM_SYSKEYDOWN;

    // Check for Ctrl+Escape exit condition.
    if is_key_down && vk_code.0 == 0x1B {
        // VK_ESCAPE
        let ctrl_state = unsafe { GetKeyState(VK_CONTROL.0 as i32) };
        if ctrl_state < 0 {
            unsafe { PostQuitMessage(0) };
            return unsafe {
                CallNextHookEx(HHOOK::default(), code, w_param, l_param)
            };
        }
    }

    // Print on key down.
    if is_key_down {
        let (name, code_str) = if let Some(key) = Key::from_native(vk_code.0) {
            (key.as_str().to_string(), format!("{}", key.as_native()))
        } else {
            // Try to get a character representation.
            let char_code = unsafe {
                MapVirtualKeyW(vk_code.0 as u32, windows::Win32::UI::Input::KeyboardAndMouse::MAP_VIRTUAL_KEY_TYPE(2))
            };
            let name = if char_code != 0 && (char_code as u8) as char != '\0' {
                format!("Unknown({}, {})", vk_code.0, char_code as u8 as char)
            } else {
                format!("Unknown({})", vk_code.0)
            };
            (name, format!("{}", vk_code.0))
        };

        println!("{name}: {code_str}");
    }

    unsafe { CallNextHookEx(HHOOK::default(), code, w_param, l_param) }
}
