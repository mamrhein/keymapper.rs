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
use windows_sys::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
            KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, MSG,
            SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
            WM_KEYDOWN, WM_SYSKEYDOWN,
        },
    },
};

use super::key::Key;
use crate::{
    common::modifier::ModifierRole,
    daemon::{mapping_cache::NativeKey, state::Lookup},
};

/// Type aliases for hook types not re-exported in windows-sys 0.61.
#[allow(clippy::upper_case_acronyms)]
type HHOOK = *mut std::ffi::c_void;

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
    Some(key.as_native())
}

fn is_extended_key(vk: VIRTUAL_KEY) -> bool {
    matches!(
        vk,
        0xA3 | 0xA5 | 0x21 | 0x22 | 0x23 | 0x25
            ..=0x28 | 0x2D | 0x2E | 0x6F | 0x92
    )
}

fn simulate_key_event(vk: VIRTUAL_KEY, is_key_up: bool) {
    let mut flags = if is_key_up { KEYEVENTF_KEYUP } else { 0 };
    if is_extended_key(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }

    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
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

    simulate_key_event(native_key.base as VIRTUAL_KEY, false);
    thread::sleep(Duration::from_millis(1));

    simulate_key_event(native_key.base as VIRTUAL_KEY, true);
    thread::sleep(Duration::from_millis(1));

    for vk in pressed_modifiers.into_iter().rev() {
        simulate_key_event(vk, true);
        thread::sleep(Duration::from_millis(1));
    }
}

/// Map a raw VIRTUAL_KEY to its modifier bit position via the shared
/// `ModifierRole` type.
fn vk_to_modifier_bit(vk: VIRTUAL_KEY) -> Option<u8> {
    let role = match vk {
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
static HOOK_HANDLE: parking_lot::Mutex<isize> = parking_lot::Mutex::new(0);

fn set_shared_lookup(lookup: Arc<RwLock<dyn Lookup>>) {
    *SHARED_LOOKUP.lock() = Some(lookup);
}

fn get_shared_lookup() -> Option<Arc<RwLock<dyn Lookup>>> {
    SHARED_LOOKUP.lock().clone()
}

fn set_hook_handle(handle: HHOOK) {
    *HOOK_HANDLE.lock() = handle as isize;
}

fn hook_handle() -> HHOOK {
    *HOOK_HANDLE.lock() as _
}

/// Clears hook callback state so tests can run in isolation.
///
/// Windows `WH_KEYBOARD_LL` requires module-level statics because the hook
/// callback cannot capture user data. This function resets both statics to
/// their initial state.
#[cfg(test)]
pub fn reset_for_tests() {
    *SHARED_LOOKUP.lock() = None;
    *HOOK_HANDLE.lock() = 0;
}

pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    set_shared_lookup(lookup);

    let h_instance: HINSTANCE =
        unsafe { GetModuleHandleW(std::ptr::null::<u16>()) };

    let handle: HHOOK = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(low_level_keyboard_proc),
            h_instance,
            0,
        )
    };

    if handle.is_null() {
        return Err("Failed to install global keyboard hook".into());
    }
    set_hook_handle(handle);
    println!("Windows low-level hook listening.");

    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {}
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

    let Some(lookup) = get_shared_lookup() else {
        return unsafe {
            CallNextHookEx(hook_handle(), code, w_param, l_param)
        };
    };

    let kbd_struct = unsafe { *(l_param as *const KBDLLHOOKSTRUCT) };
    let vk_code = kbd_struct.vkCode as VIRTUAL_KEY;

    let is_key_down =
        w_param as u32 == WM_KEYDOWN || w_param as u32 == WM_SYSKEYDOWN;

    // Clear the current key's modifier bit from the polled state so that
    // bare-modifier triggers (e.g. "LeftControl: A") match correctly against
    // the concurrent modifier set.
    let mut pressed_modifiers = extract_modifier_bits();
    if let Some(bit) = vk_to_modifier_bit(vk_code) {
        pressed_modifiers &= !(1 << bit);
    }

    let guard = lookup.read();
    let active_outputs = guard
        .for_app(&guard.active_app(), vk_code, pressed_modifiers, None)
        .or_else(|| guard.global(vk_code, pressed_modifiers, None))
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
        return 1; // Swallow the original key
    }

    unsafe { CallNextHookEx(hook_handle(), code, w_param, l_param) }
}
