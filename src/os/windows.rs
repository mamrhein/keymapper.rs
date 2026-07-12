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
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use windows_sys::Windows::Win32::{
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
            KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, HHOOK, HINSTANCE, KBDLLHOOKSTRUCT,
            LPARAM, LRESULT, MSG, SetWindowsHookExW, UnhookWindowsHookEx,
            WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
            WPARAM,
        },
    },
};

use crate::{key_names, mapping_cache::NativeAction, state::Lookup};

// ---------------------------------------------------------------------------
// Platform-specific Key enum — discriminants ARE the VK_* codes
// ---------------------------------------------------------------------------

/// Windows virtual-key code for a physical key on a US ANSI keyboard.
///
/// Discriminant values come from `<WinUser.h>` (`VK_*` constants).
/// `key as u16` yields the native VIRTUAL_KEY — no translation needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Key {
    // --- Modifiers ---
    LeftControl = 0xA2,  // VK_LCONTROL
    RightControl = 0xA3, // VK_RCONTROL
    LeftShift = 0xA0,    // VK_LSHIFT
    RightShift = 0xA1,   // VK_RSHIFT
    LeftAlt = 0xA4,      // VK_LMENU
    RightAlt = 0xA5,     // VK_RMENU
    LeftCommand = 0x5B,  // VK_LWIN
    RightCommand = 0x5C, // VK_RWIN
    CapsLock = 0x14,     // VK_CAPITAL
    // --- Editor / misc ---
    Tab = 0x09,       // VK_TAB
    Space = 0x20,     // VK_SPACE
    Return = 0x0D,    // VK_RETURN
    Backspace = 0x08, // VK_BACK
    Delete = 0x2E,    // VK_DELETE
    Escape = 0x1B,    // VK_ESCAPE
    // --- Navigation ---
    UpArrow = 0x26,    // VK_UP
    DownArrow = 0x28,  // VK_DOWN
    LeftArrow = 0x25,  // VK_LEFT
    RightArrow = 0x27, // VK_RIGHT
    PageUp = 0x21,     // VK_PRIOR
    PageDown = 0x22,   // VK_NEXT
    Home = 0x23,       // VK_HOME
    End = 0x23,        // VK_END (shares VK code with Home)
    // --- Function keys ---
    F1 = 0x70,  // VK_F1
    F2 = 0x71,  // VK_F2
    F3 = 0x72,  // VK_F3
    F4 = 0x73,  // VK_F4
    F5 = 0x74,  // VK_F5
    F6 = 0x75,  // VK_F6
    F7 = 0x76,  // VK_F7
    F8 = 0x77,  // VK_F8
    F9 = 0x78,  // VK_F9
    F10 = 0x79, // VK_F10
    F11 = 0x7A, // VK_F11
    F12 = 0x7B, // VK_F12
    // --- Letters ---
    A = 0x41, // VK_A
    B = 0x42, // VK_B
    C = 0x43, // VK_C
    D = 0x44, // VK_D
    E = 0x45, // VK_E
    F = 0x46, // VK_F
    G = 0x47, // VK_G
    H = 0x48, // VK_H
    I = 0x49, // VK_I
    J = 0x4A, // VK_J
    K = 0x4B, // VK_K
    L = 0x4C, // VK_L
    M = 0x4D, // VK_M
    N = 0x4E, // VK_N
    O = 0x4F, // VK_O
    P = 0x50, // VK_P
    Q = 0x51, // VK_Q
    R = 0x52, // VK_R
    S = 0x53, // VK_S
    T = 0x54, // VK_T
    U = 0x55, // VK_U
    V = 0x56, // VK_V
    W = 0x57, // VK_W
    X = 0x58, // VK_X
    Y = 0x59, // VK_Y
    Z = 0x5A, // VK_Z
    // --- Numbers ---
    Number1 = 0x31, // VK_1
    Number2 = 0x32, // VK_2
    Number3 = 0x33, // VK_3
    Number4 = 0x34, // VK_4
    Number5 = 0x35, // VK_5
    Number6 = 0x36, // VK_6
    Number7 = 0x37, // VK_7
    Number8 = 0x38, // VK_8
    Number9 = 0x39, // VK_9
    Number0 = 0x30, // VK_0
}

impl Key {
    /// Convert to the native VIRTUAL_KEY.  Zero-cost — the discriminant IS the
    /// code.
    pub const fn as_native(self) -> VIRTUAL_KEY {
        self as VIRTUAL_KEY
    }

    /// Return the canonical config-name for this key.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LeftControl => "leftcontrol",
            Self::RightControl => "rightcontrol",
            Self::LeftShift => "leftshift",
            Self::RightShift => "rightshift",
            Self::LeftAlt => "leftalt",
            Self::RightAlt => "rightalt",
            Self::LeftCommand => "leftcommand",
            Self::RightCommand => "rightcommand",
            Self::CapsLock => "capslock",
            Self::Tab => "tab",
            Self::Space => "space",
            Self::Return => "return",
            Self::Backspace => "backspace",
            Self::Delete => "delete",
            Self::Escape => "escape",
            Self::UpArrow => "uparrow",
            Self::DownArrow => "downarrow",
            Self::LeftArrow => "leftarrow",
            Self::RightArrow => "rightarrow",
            Self::PageUp => "pageup",
            Self::PageDown => "pagedown",
            Self::Home => "home",
            Self::End => "end",
            Self::F1 => "f1",
            Self::F2 => "f2",
            Self::F3 => "f3",
            Self::F4 => "f4",
            Self::F5 => "f5",
            Self::F6 => "f6",
            Self::F7 => "f7",
            Self::F8 => "f8",
            Self::F9 => "f9",
            Self::F10 => "f10",
            Self::F11 => "f11",
            Self::F12 => "f12",
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
            Self::D => "d",
            Self::E => "e",
            Self::F => "f",
            Self::G => "g",
            Self::H => "h",
            Self::I => "i",
            Self::J => "j",
            Self::K => "k",
            Self::L => "l",
            Self::M => "m",
            Self::N => "n",
            Self::O => "o",
            Self::P => "p",
            Self::Q => "q",
            Self::R => "r",
            Self::S => "s",
            Self::T => "t",
            Self::U => "u",
            Self::V => "v",
            Self::W => "w",
            Self::X => "x",
            Self::Y => "y",
            Self::Z => "z",
            Self::Number1 => "1",
            Self::Number2 => "2",
            Self::Number3 => "3",
            Self::Number4 => "4",
            Self::Number5 => "5",
            Self::Number6 => "6",
            Self::Number7 => "7",
            Self::Number8 => "8",
            Self::Number9 => "9",
            Self::Number0 => "0",
        }
    }

    /// Parse a canonical name into a Key variant.
    pub fn from_canonical(name: &str) -> Option<Self> {
        match name {
            "leftcontrol" => Some(Self::LeftControl),
            "rightcontrol" => Some(Self::RightControl),
            "leftshift" => Some(Self::LeftShift),
            "rightshift" => Some(Self::RightShift),
            "leftalt" => Some(Self::LeftAlt),
            "rightalt" => Some(Self::RightAlt),
            "leftcommand" => Some(Self::LeftCommand),
            "rightcommand" => Some(Self::RightCommand),
            "capslock" => Some(Self::CapsLock),
            "tab" => Some(Self::Tab),
            "space" => Some(Self::Space),
            "return" => Some(Self::Return),
            "backspace" => Some(Self::Backspace),
            "delete" => Some(Self::Delete),
            "escape" => Some(Self::Escape),
            "uparrow" => Some(Self::UpArrow),
            "downarrow" => Some(Self::DownArrow),
            "leftarrow" => Some(Self::LeftArrow),
            "rightarrow" => Some(Self::RightArrow),
            "pageup" => Some(Self::PageUp),
            "pagedown" => Some(Self::PageDown),
            "home" => Some(Self::Home),
            "end" => Some(Self::End),
            "f1" => Some(Self::F1),
            "f2" => Some(Self::F2),
            "f3" => Some(Self::F3),
            "f4" => Some(Self::F4),
            "f5" => Some(Self::F5),
            "f6" => Some(Self::F6),
            "f7" => Some(Self::F7),
            "f8" => Some(Self::F8),
            "f9" => Some(Self::F9),
            "f10" => Some(Self::F10),
            "f11" => Some(Self::F11),
            "f12" => Some(Self::F12),
            "a" => Some(Self::A),
            "b" => Some(Self::B),
            "c" => Some(Self::C),
            "d" => Some(Self::D),
            "e" => Some(Self::E),
            "f" => Some(Self::F),
            "g" => Some(Self::G),
            "h" => Some(Self::H),
            "i" => Some(Self::I),
            "j" => Some(Self::J),
            "k" => Some(Self::K),
            "l" => Some(Self::L),
            "m" => Some(Self::M),
            "n" => Some(Self::N),
            "o" => Some(Self::O),
            "p" => Some(Self::P),
            "q" => Some(Self::Q),
            "r" => Some(Self::R),
            "s" => Some(Self::S),
            "t" => Some(Self::T),
            "u" => Some(Self::U),
            "v" => Some(Self::V),
            "w" => Some(Self::W),
            "x" => Some(Self::X),
            "y" => Some(Self::Y),
            "z" => Some(Self::Z),
            "1" | "number1" => Some(Self::Number1),
            "2" | "number2" => Some(Self::Number2),
            "3" | "number3" => Some(Self::Number3),
            "4" | "number4" => Some(Self::Number4),
            "5" | "number5" => Some(Self::Number5),
            "6" | "number6" => Some(Self::Number6),
            "7" | "number7" => Some(Self::Number7),
            "8" | "number8" => Some(Self::Number8),
            "9" | "number9" => Some(Self::Number9),
            "0" | "number0" => Some(Self::Number0),
            _ => None,
        }
    }
}

impl Serialize for Key {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let l = s.to_lowercase();
        let canonical = key_names::resolve_alias(&l).unwrap_or(&l);
        Self::from_canonical(canonical).ok_or_else(|| {
            serde::de::Error::custom(key_names::unknown_key_error(&s))
        })
    }
}

// ---------------------------------------------------------------------------
// Low-level keyboard hook
// ---------------------------------------------------------------------------

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

/// Return true when the given virtual-key code corresponds to an extended
/// hardware key (scan-code prefixed with 0xE0).  These include the right-side
/// modifiers, navigation cluster (arrows / Home / End / Ins / Del / PgUp /
/// PgDown), and the numpad Enter.
fn is_extended_key(vk: VIRTUAL_KEY) -> bool {
    matches!(
        vk,
        // Right-side modifiers
        0xA3 | 0xA5 // VK_RCONTROL, VK_RMENU
            // Navigation cluster
            | 0x21 | 0x22 | 0x23 | 0x25
            ..=0x28 // PgUp, PgDn, Home/End, arrows
            | 0x2D | 0x2E // VK_INSERT, VK_DELETE
    )
}

/// Inject a synthetic key event via `SendInput` (modern replacement for
/// the deprecated `keybd_event`).  `vk` is `VIRTUAL_KEY` (u16) — matching
/// both `NativeKey` and the API natively.
fn simulate_key_event(vk: VIRTUAL_KEY, is_key_up: bool) {
    let mut flags = if is_key_up { KEYEVENTF_KEYUP } else { 0 };
    if is_extended_key(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }

    let mut input = INPUT {
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
