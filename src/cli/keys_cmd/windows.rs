// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows implementation of `keymapper keys probe`.

use windows::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL},
        WindowsAndMessaging::{
            CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK,
            KBDLLHOOKSTRUCT, MSG, PostQuitMessage, SetWindowsHookExW,
            TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN,
            WM_SYSKEYDOWN,
        },
    },
};

use crate::platform::keycode_to_hid_usage;

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
            Some(h_instance),
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
            // TranslateMessage's return value is not used; the message is
            // always dispatched regardless of whether it was
            // translated.
            let _ = TranslateMessage(&msg);
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
            CallNextHookEx(Some(HHOOK::default()), code, w_param, l_param)
        };
    }

    let kbd_struct = unsafe { &*(l_param.0 as *const KBDLLHOOKSTRUCT) };
    let vk_code = kbd_struct.vkCode as u16;

    let is_key_down =
        w_param.0 as u32 == WM_KEYDOWN || w_param.0 as u32 == WM_SYSKEYDOWN;

    // Check for Ctrl+Escape exit condition.
    if is_key_down && vk_code == 0x1B {
        // VK_ESCAPE
        let ctrl_state = unsafe { GetKeyState(VK_CONTROL.0 as i32) };
        if ctrl_state < 0 {
            unsafe { PostQuitMessage(0) };
            return unsafe {
                CallNextHookEx(Some(HHOOK::default()), code, w_param, l_param)
            };
        }
    }

    // Print on key down.  The canonical name and the HID usage id are
    // resolved via the shared `HidUsage` type, matching the other
    // platforms' probe output.
    if is_key_down {
        let (name, code_str) = match keycode_to_hid_usage(vk_code) {
            Some(usage) => {
                (usage.as_str().to_string(), format!("0x{:02X}", usage.id()))
            }
            None => (format!("Unknown({vk_code})"), format!("{vk_code}")),
        };

        println!("{name}: {code_str}");
    }

    unsafe { CallNextHookEx(Some(HHOOK::default()), code, w_param, l_param) }
}
