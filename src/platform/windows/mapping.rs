// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{sync::Arc, thread, time::Duration};

use parking_lot::RwLock;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, MSG,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_SYSKEYDOWN,
};

use super::key::Key;
use crate::{
    common::modifier::ModifierRole,
    daemon::{mapping_cache::NativeKey, state::Lookup},
};

// ---------------------------------------------------------------------------
// Modifier handling
// ---------------------------------------------------------------------------

fn extract_modifier_bits() -> u8 {
    let mut bits: u8 = 0;
    if unsafe { GetAsyncKeyState(Key::LeftControl.as_native() as i32) } < 0 {
        bits |= ModifierRole::LeftControl.mask();
    }
    if unsafe { GetAsyncKeyState(Key::RightControl.as_native() as i32) } < 0 {
        bits |= ModifierRole::RightControl.mask();
    }
    if unsafe { GetAsyncKeyState(Key::LeftShift.as_native() as i32) } < 0 {
        bits |= ModifierRole::LeftShift.mask();
    }
    if unsafe { GetAsyncKeyState(Key::RightShift.as_native() as i32) } < 0 {
        bits |= ModifierRole::RightShift.mask();
    }
    if unsafe { GetAsyncKeyState(Key::LeftAlt.as_native() as i32) } < 0 {
        bits |= ModifierRole::LeftAlt.mask();
    }
    if unsafe { GetAsyncKeyState(Key::RightAlt.as_native() as i32) } < 0 {
        bits |= ModifierRole::RightAlt.mask();
    }
    if unsafe { GetAsyncKeyState(Key::LeftCommand.as_native() as i32) } < 0 {
        bits |= ModifierRole::LeftCommand.mask();
    }
    if unsafe { GetAsyncKeyState(Key::RightCommand.as_native() as i32) } < 0 {
        bits |= ModifierRole::RightCommand.mask();
    }
    bits
}

/// Map a modifier bit position back to the native VIRTUAL_KEY for emission.
fn modifier_bit_to_vk(bit: u8) -> Option<VIRTUAL_KEY> {
    let role = ModifierRole::try_from_bit(bit)?;
    let key = match role {
        ModifierRole::LeftControl => Key::LeftControl,
        ModifierRole::RightControl => Key::RightControl,
        ModifierRole::LeftShift => Key::LeftShift,
        ModifierRole::RightShift => Key::RightShift,
        ModifierRole::LeftAlt => Key::LeftAlt,
        ModifierRole::RightAlt => Key::RightAlt,
        ModifierRole::LeftCommand => Key::LeftCommand,
        ModifierRole::RightCommand => Key::RightCommand,
    };
    Some(VIRTUAL_KEY(key.as_native()))
}

fn is_extended_key(vk: VIRTUAL_KEY) -> bool {
    matches!(
        vk.0,
        0xA3 | 0xA5 | 0x21 | 0x22 | 0x23 | 0x25
            ..=0x28 | 0x2D | 0x2E | 0x6F | 0x92
    )
}

fn simulate_key_event(vk: VIRTUAL_KEY, is_key_up: bool) {
    let mut flags: u32 = if is_key_up { KEYEVENTF_KEYUP.0 } else { 0 };
    if is_extended_key(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY.0;
    }

    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(flags),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

fn emit_key_event(native_key: &NativeKey) {
    let mut pressed_modifiers: Vec<VIRTUAL_KEY> = Vec::new();

    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1
            && let Some(vk) = modifier_bit_to_vk(bit)
        {
            simulate_key_event(vk, false);
            pressed_modifiers.push(vk);
            thread::sleep(Duration::from_millis(1));
        }
    }

    simulate_key_event(VIRTUAL_KEY(native_key.base), false);
    thread::sleep(Duration::from_millis(1));

    simulate_key_event(VIRTUAL_KEY(native_key.base), true);
    thread::sleep(Duration::from_millis(1));

    for vk in pressed_modifiers.into_iter().rev() {
        simulate_key_event(vk, true);
        thread::sleep(Duration::from_millis(1));
    }
}

/// Map a raw VIRTUAL_KEY to its modifier bit position via the shared
/// `ModifierRole` type.
fn vk_to_modifier_bit(vk: VIRTUAL_KEY) -> Option<u8> {
    let role = match vk.0 {
        0xA2 => ModifierRole::LeftControl,
        0xA3 => ModifierRole::RightControl,
        0xA0 => ModifierRole::LeftShift,
        0xA1 => ModifierRole::RightShift,
        0xA4 => ModifierRole::LeftAlt,
        0xA5 => ModifierRole::RightAlt,
        0x5B => ModifierRole::LeftCommand,
        0x5C => ModifierRole::RightCommand,
        _ => return None,
    };
    Some(role.bit())
}

// ---------------------------------------------------------------------------
// Low-level keyboard hook
// ---------------------------------------------------------------------------

static SHARED_LOOKUP: parking_lot::Mutex<Option<Arc<RwLock<dyn Lookup>>>> =
    parking_lot::Mutex::new(None);

// `HHOOK` wraps a raw `*mut c_void` which is not `Send`.  We use a raw
// pointer stored in a usize instead, which is `Send` and `Sync`.  This is
// safe because the hook handle is only ever read/written through the mutex.
type RawHookHandle = usize;

static HOOK_HANDLE: parking_lot::Mutex<RawHookHandle> =
    parking_lot::Mutex::new(0);

fn set_shared_lookup(lookup: Arc<RwLock<dyn Lookup>>) {
    *SHARED_LOOKUP.lock() = Some(lookup);
}

fn get_shared_lookup() -> Option<Arc<RwLock<dyn Lookup>>> {
    SHARED_LOOKUP.lock().clone()
}

fn set_hook_handle(handle: HHOOK) {
    *HOOK_HANDLE.lock() = handle.0 as RawHookHandle;
}

fn hook_handle() -> HHOOK {
    HHOOK(*HOOK_HANDLE.lock() as *mut std::ffi::c_void)
}

pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    _device_path_override: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Windows uses a global keyboard hook; device path is ignored.
    set_shared_lookup(lookup);

    let h_instance: HINSTANCE = unsafe { GetModuleHandleW(None)?.into() };

    let handle = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(low_level_keyboard_proc),
            h_instance,
            0,
        )?
    };

    if handle.is_invalid() {
        return Err("Failed to install global keyboard hook".into());
    }
    set_hook_handle(handle);
    println!("Windows low-level hook listening.");

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        UnhookWindowsHookEx(hook_handle())?;
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

    let Some(lookup) = get_shared_lookup() else {
        return unsafe {
            CallNextHookEx(hook_handle(), code, w_param, l_param)
        };
    };

    let kbd_struct = unsafe { &*(l_param.0 as *const KBDLLHOOKSTRUCT) };
    let vk_code = VIRTUAL_KEY(kbd_struct.vkCode as u16);

    let is_key_down =
        w_param.0 as u32 == WM_KEYDOWN || w_param.0 as u32 == WM_SYSKEYDOWN;

    // Clear the current key's modifier bit from the polled state so that
    // bare-modifier triggers (e.g. "LeftControl: A") match correctly against
    // the concurrent modifier set.
    let mut pressed_modifiers = extract_modifier_bits();
    if let Some(bit) = vk_to_modifier_bit(vk_code) {
        pressed_modifiers &= !(1 << bit);
    }

    let guard = lookup.read();
    let active_outputs = guard
        .for_app(&guard.active_app(), vk_code.0, pressed_modifiers, None)
        .or_else(|| guard.global(vk_code.0, pressed_modifiers, None))
        .map(|v| v.to_vec());
    drop(guard);

    if let Some(outputs) = active_outputs {
        // Emit mapped outputs and swallow the original event.  This applies
        // to modifier keys as well: if a bare modifier is mapped, its outputs
        // are emitted and the original key is NOT passed to the next hook.
        if is_key_down {
            for native_key in &outputs {
                emit_key_event(native_key);
            }
        }
        return LRESULT(1); // Swallow the original key
    }

    unsafe { CallNextHookEx(hook_handle(), code, w_param, l_param) }
}
