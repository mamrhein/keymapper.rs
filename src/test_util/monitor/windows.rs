// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows hook-capture backend for the keyboard monitor.
//!
//! Instead of a focused GUI window — whose capture depends on the window
//! manager keeping keyboard focus on it, and which is therefore brittle and
//! steals focus from the user on an interactive session — the Windows
//! monitor installs a `WH_KEYBOARD_LL` hook and captures every key that
//! reaches the session's hook chain.  The harness starts the daemon before
//! the monitor, so the daemon's hook (installed first) swallows remapped
//! inputs before this hook sees them: what remains is exactly the daemon's
//! re-emitted outputs plus forwarded passthroughs (including the `Ctrl+Esc`
//! round delimiter), never the raw injected inputs.  The hook logs every
//! captured key and swallows it, so nothing leaks into the compositor or any
//! focused window.  This mirrors the Linux direct-capture backend: it is
//! deterministic, needs no window or keyboard focus, and is headless
//! friendly.

use std::sync::atomic::Ordering;

use windows::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW,
        UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
    },
};

use super::{OutputEvent, register_signal_handlers, writer::EventWriter};
use crate::platform::Key;

/// The writer is set once from [`run`] before the hook is installed, and
/// written from the hook callback.  A `Mutex` guards it because the hook
/// callback is a `fn` pointer with no captured state, so the writer must live
/// in a `static`.  Access is effectively uncontended: only the hook thread
/// writes.
static WRITER: std::sync::OnceLock<parking_lot::Mutex<EventWriter>> =
    std::sync::OnceLock::new();

/// Low-level keyboard hook used by the monitor.
///
/// Every key reaching the hook chain is captured and swallowed (see the
/// module docs for why that is exactly the daemon's output).
unsafe extern "system" fn monitor_hook_proc(
    code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, w_param, l_param) };
    }

    let kbd_struct = unsafe { &*(l_param.0 as *const KBDLLHOOKSTRUCT) };

    let is_key_down =
        w_param.0 as u32 == WM_KEYDOWN || w_param.0 as u32 == WM_SYSKEYDOWN;

    // Log the captured key under its exact name (left/right sides and Super
    // are distinguished), matching the daemon's emission.
    if let Some(key) =
        Key::from_native(kbd_struct.vkCode as u16).map(Key::to_hid_usage)
        && let Some(writer) = WRITER.get()
        && let Ok(()) = writer.lock().write(OutputEvent {
            down: is_key_down,
            key,
        })
    {
        // Written and flushed.
    }

    // Swallow the key so it does not leak to a focused window — the "grab"
    // semantics of the Linux direct-capture backend.
    LRESULT(1)
}

/// Entry point for the Windows hook-capture monitor.
///
/// Installs a `WH_KEYBOARD_LL` hook on the current thread and runs a message
/// loop until the process is terminated.  E2E tests kill the process with a
/// hard `TerminateProcess`; because events are written synchronously in the
/// hook callback (and the daemon is stopped before the monitor), no captured
/// event is lost on shutdown.
pub fn run(sink: EventWriter) {
    if WRITER.set(parking_lot::Mutex::new(sink)).is_err() {
        panic!("monitor writer already initialized");
    }

    let shutdown = register_signal_handlers();

    let h_instance: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .expect("GetModuleHandleW failed")
        .into();

    let hook = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(monitor_hook_proc),
            Some(h_instance),
            0,
        )
    }
    .expect("failed to install monitor keyboard hook");

    if hook.is_invalid() {
        panic!("monitor keyboard hook handle is invalid");
    }

    eprintln!("monitor: capturing daemon-emitted keys via WH_KEYBOARD_LL");

    // Run the message loop that delivers the hook.  It exits when the process
    // is killed (hard terminate in e2e) or the shutdown flag is set.
    unsafe {
        let mut msg = MSG::default();
        while !shutdown.load(Ordering::Relaxed)
            && GetMessageW(&mut msg, None, 0, 0).as_bool()
        {
            // Pump one message.
        }
        let _ = UnhookWindowsHookEx(hook);
    }
}
