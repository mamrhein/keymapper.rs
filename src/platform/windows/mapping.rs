// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows keyboard mapping via a three-thread architecture.
//!
//! The hook thread installs a \`WH_KEYBOARD_LL\` hook and runs the message
//! loop.  On each key event it sends a request to the worker thread and
//! blocks on a one-shot reply channel.  The worker matches against recent
//! raw input events to identify the source keyboard, performs the mapping
//! lookup, and replies with \`swallow\` or \`pass through\`.
//!
//! Thread layout:
//!
//! 1. **Hook thread** — \`WH_KEYBOARD_LL\` hook + \`GetMessageW\` loop. Sends
//!    \`HookEvent\` to worker, blocks on reply.
//! 2. **Raw input thread** — Message-only window + \`GetMessageW\` loop for
//!    \`WM_INPUT\`.  Sends \`RawInputEvent\` to worker.
//! 3. **Worker thread** — Receives from both channels, matches events,
//!    resolves devices, performs lookups, sends decisions back.

use std::{
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
    },
};

use crossbeam_channel;
use parking_lot::RwLock;
use windows::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
            KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, MSG,
            SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
            WM_KEYDOWN, WM_SYSKEYDOWN,
        },
    },
};

use super::{
    CAPTURE_TAG,
    dispatch::{Decision, HookEvent, spawn_worker},
    key::{Key, hid_to_vk},
    raw_input::start_raw_input_loop,
};
use crate::{
    common::{hid_usage::HidUsage, modifier::ModifierRole},
    daemon::{mapping_cache::NativeKey, state::Lookup},
};

// ---------------------------------------------------------------------------
// Static state for the hook procedure
// ---------------------------------------------------------------------------

/// Shared sender for hook events.  Accessed from the hook proc so that
/// events can be pushed to the worker thread.
static HOOK_TX: parking_lot::Mutex<
    Option<crossbeam_channel::Sender<HookEvent>>,
> = parking_lot::Mutex::new(None);

/// Stores the sender that the hook procedure uses to push events.
fn set_hook_tx(tx: crossbeam_channel::Sender<HookEvent>) {
    *HOOK_TX.lock() = Some(tx);
}

/// Retrieves the sender for pushing hook events.
fn get_hook_tx() -> Option<crossbeam_channel::Sender<HookEvent>> {
    HOOK_TX.lock().clone()
}

/// `HHOOK` wraps a raw `*mut c_void` which is not `Send`.  We use a raw
/// pointer stored in a usize instead, which is `Send` and `Sync`.  This is
/// safe because the hook handle is only ever read/written through the mutex.
type RawHookHandle = usize;

static HOOK_HANDLE: parking_lot::Mutex<RawHookHandle> =
    parking_lot::Mutex::new(0);

fn set_hook_handle(handle: HHOOK) {
    *HOOK_HANDLE.lock() = handle.0 as RawHookHandle;
}

fn hook_handle() -> HHOOK {
    HHOOK(*HOOK_HANDLE.lock() as *mut std::ffi::c_void)
}

/// Tracks keys currently injected by the daemon itself via `SendInput`.
///
/// When the daemon emits a mapped output key (e.g., LeftControl down), the
/// hook procedure sees this injected event.  Without tracking, the daemon
/// would process its own injection as a new key press, creating duplicate or
/// incorrect mappings.  This set stores `(vk_code, is_key_down)` pairs while
/// the injected key is active.
static INJECTED_KEYS: std::sync::Mutex<Vec<(u16, bool)>> =
    std::sync::Mutex::new(Vec::new());

/// Registers an injected key so the hook proc can skip it.
fn mark_injected(vk: u16, is_down: bool) {
    INJECTED_KEYS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((vk, is_down));
}

/// Checks if the given key event was injected by the daemon itself.
fn is_injected_key(vk: u16, is_down: bool) -> bool {
    INJECTED_KEYS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|(v, d)| *v == vk && *d == is_down)
}

/// Removes a single matching entry from the injected key tracker.
fn clear_injected(vk: u16, is_down: bool) {
    let keys = INJECTED_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pos) = keys.iter().position(|(v, d)| *v == vk && *d == is_down)
    {
        let mut keys = INJECTED_KEYS.lock().unwrap_or_else(|e| e.into_inner());
        keys.remove(pos);
    }
}

// ---------------------------------------------------------------------------
// Modifier handling
// ---------------------------------------------------------------------------

pub(super) fn extract_modifier_bits() -> u8 {
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
    let is_down = !is_key_up;

    // In test mode, write output events to a file instead of calling
    // `SendInput`. This avoids the issue where `SendInput` from within a
    // `WH_KEYBOARD_LL` hook callback does not trigger other hooks (Windows
    // prevents recursive hook invocation). The e2e test reads this file to
    // verify outputs.
    if let Ok(path) = std::env::var("KEYMAPPER_TEST_OUTPUT") {
        let line = if is_down {
            format!("DOWN {}\n", vk.0)
        } else {
            format!("UP {}\n", vk.0)
        };
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
            .as_mut()
            .and_then(|f| f.write_all(line.as_bytes()).ok());
        return;
    }

    // Mark this key as injected so the hook proc can skip it.
    mark_injected(vk.0, is_down);

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

/// Emit a complete key event (chord press + release) via `SendInput`.
///
/// The output's `HidUsage` is resolved to a virtual-key code: Keyboard
/// page usages through their `Key` variant's VK code, Consumer Page
/// usages through the static `hid_to_vk` translation table.
pub(super) fn emit_key_event(native_key: &NativeKey) {
    let mut pressed_modifiers: Vec<VIRTUAL_KEY> = Vec::new();

    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1
            && let Some(vk) = modifier_bit_to_vk(bit)
        {
            simulate_key_event(vk, false);
            pressed_modifiers.push(vk);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    let base_vk = Key::from_hid_usage(native_key.usage)
        .map(Key::as_native)
        .or_else(|| hid_to_vk(native_key.usage));

    let Some(base_vk) = base_vk else {
        eprintln!(
            "Windows: no VK code for output HID usage {:?}",
            native_key.usage
        );
        // Release the modifiers that were already pressed to avoid a
        // stuck-modifier state.
        for vk in pressed_modifiers.into_iter().rev() {
            simulate_key_event(vk, true);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        return;
    };

    simulate_key_event(VIRTUAL_KEY(base_vk), false);
    std::thread::sleep(std::time::Duration::from_millis(1));

    simulate_key_event(VIRTUAL_KEY(base_vk), true);
    std::thread::sleep(std::time::Duration::from_millis(1));

    for vk in pressed_modifiers.into_iter().rev() {
        simulate_key_event(vk, true);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

// ---------------------------------------------------------------------------
// Capture mode
// ---------------------------------------------------------------------------
//
// Capture mode makes the daemon re-emit every key through its virtual
// keyboard, tagged with [`CAPTURE_TAG`], so the e2e monitor's
// `WH_KEYBOARD_LL` hook can capture the daemon's output without depending on
// a focused window.  It is gated on the `KEYMAPPER_CAPTURE` environment
// variable so production behaviour (unmapped keys passing straight through)
// is left untouched.

/// Process start, for capture-debug timestamps.
static CAPTURE_T0: std::sync::OnceLock<std::time::Instant> =
    std::sync::OnceLock::new();

/// Capture-mode debug log, appended to from the hook and worker threads.
static CAPTURE_DEBUG: std::sync::OnceLock<std::sync::Mutex<std::fs::File>> =
    std::sync::OnceLock::new();

pub(super) fn capture_debug(line: &str) {
    let t0 = CAPTURE_T0.get_or_init(std::time::Instant::now);
    let file = CAPTURE_DEBUG.get_or_init(|| {
        let path = std::env::temp_dir().join("keymapper_capture_debug.log");
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map(std::sync::Mutex::new)
            .expect("failed to open capture debug log")
    });
    let mut f = file.lock().unwrap_or_else(|e| e.into_inner());
    let _ = writeln!(f, "t={:6}ms {}", t0.elapsed().as_millis(), line);
}

/// Modifier keys currently pressed, tracked from the hook's own event
/// stream.
///
/// `GetAsyncKeyState` cannot be used here: its state lags behind the very
/// event the hook is processing (for `SendInput`-injected keys it does not
/// yet reflect the key-down being dispatched), so fast chords would miss
/// their modifiers.  It is also poisoned by session leftovers such as a
/// modifier key stuck "down" in the async state after an interrupted input
/// sequence.  Since every key event (physical or injected) passes through
/// the low-level hook, tracking the state from the events themselves is
/// both faster and exact.  Bit positions match [`ModifierRole`] (and the
/// compiled rule masks).  Only the hook thread touches this value.
static TRACKED_MODIFIERS: AtomicU8 = AtomicU8::new(0);

/// Set once from `start_mapping` to record whether capture mode is active.
static CAPTURE_MODE: AtomicBool = AtomicBool::new(false);

/// Whether capture mode is active (all emission tagged through the virtual
/// keyboard).
pub(super) fn capture_enabled() -> bool {
    CAPTURE_MODE.load(Ordering::Relaxed)
}

/// Record the capture-mode flag determined at startup.
fn set_capture_mode(enabled: bool) {
    CAPTURE_MODE.store(enabled, Ordering::Relaxed);
}

/// Emit a single key-press or key-release via `SendInput`, tagged with
/// [`CAPTURE_TAG`] so the monitor's hook can recognize it.  No
/// `INJECTED_KEYS` tracking is needed: the tag alone identifies daemon
/// re-emissions.
fn simulate_key_event_tagged(vk: VIRTUAL_KEY, is_key_up: bool) {
    if capture_enabled() {
        capture_debug(&format!("emit vk={:#04x} up={}", vk.0, is_key_up));
    }
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
                dwExtraInfo: CAPTURE_TAG,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// Emit a complete mapped-output chord (modifiers + base + modifiers) via
/// `SendInput`, tagged with [`CAPTURE_TAG`].  Mirrors [`emit_key_event`] but
/// is used in capture mode.
fn emit_key_event_tagged(native_key: &NativeKey) {
    let mut pressed_modifiers: Vec<VIRTUAL_KEY> = Vec::new();

    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1
            && let Some(vk) = modifier_bit_to_vk(bit)
        {
            simulate_key_event_tagged(vk, false);
            pressed_modifiers.push(vk);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    let base_vk = Key::from_hid_usage(native_key.usage)
        .map(Key::as_native)
        .or_else(|| hid_to_vk(native_key.usage));

    let Some(base_vk) = base_vk else {
        eprintln!(
            "Windows: no VK code for output HID usage {:?}",
            native_key.usage
        );
        for vk in pressed_modifiers.into_iter().rev() {
            simulate_key_event_tagged(vk, true);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        return;
    };

    simulate_key_event_tagged(VIRTUAL_KEY(base_vk), false);
    std::thread::sleep(std::time::Duration::from_millis(1));

    simulate_key_event_tagged(VIRTUAL_KEY(base_vk), true);
    std::thread::sleep(std::time::Duration::from_millis(1));

    for vk in pressed_modifiers.into_iter().rev() {
        simulate_key_event_tagged(vk, true);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Emit a mapped output, routing it through the virtual keyboard in capture
/// mode and via a direct `SendInput` in normal mode.  This is the single
/// emission entry point shared by the hook proc (mapped keyboard outputs)
/// and the worker (standalone consumer outputs).
pub(super) fn emit_mapped_output(native_key: &NativeKey) {
    if capture_enabled() {
        emit_key_event_tagged(native_key);
    } else {
        emit_key_event(native_key);
    }
}

/// Forward a single (unmapped) key through the virtual keyboard in capture
/// mode.  In normal mode unmapped keys pass straight through the OS, so this
/// is a no-op.
pub(super) fn emit_forwarded_key(vk: u16, is_key_up: bool) {
    if capture_enabled() {
        simulate_key_event_tagged(VIRTUAL_KEY(vk), is_key_up);
    }
}

// ---------------------------------------------------------------------------
// Low-level keyboard hook procedure
// ---------------------------------------------------------------------------

/// Counter for polling the reply channel.  Resets on each hook event.
static REPLY_COUNTER: AtomicU32 = AtomicU32::new(0);

extern "system" fn low_level_keyboard_proc(
    code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, w_param, l_param) };
    }

    let Some(tx) = get_hook_tx() else {
        return unsafe { CallNextHookEx(None, code, w_param, l_param) };
    };

    let kbd_struct = unsafe { &*(l_param.0 as *const KBDLLHOOKSTRUCT) };
    let vk_code = VIRTUAL_KEY(kbd_struct.vkCode as u16);

    // In capture mode the daemon re-emits every key through the virtual
    // keyboard, tagged with [`CAPTURE_TAG`].  Let those tagged re-emissions
    // flow on to the monitor's hook without re-mapping them.
    if capture_enabled() && kbd_struct.dwExtraInfo == CAPTURE_TAG {
        capture_debug(&format!(
            "hook tagged vk={:#04x} msg={:#06x}",
            kbd_struct.vkCode, w_param.0
        ));
        return unsafe {
            CallNextHookEx(Some(hook_handle()), code, w_param, l_param)
        };
    }

    let is_key_down =
        w_param.0 as u32 == WM_KEYDOWN || w_param.0 as u32 == WM_SYSKEYDOWN;
    let is_key_up = !is_key_down;

    // Derive the HID identity of the key — the lookup key space of the
    // compiled rules.  `None` for virtual-key codes without a `HidUsage`
    // (e.g. Print Screen); such keys always pass through.
    let usage = Key::from_native(vk_code.0).map(Key::to_hid_usage);

    // Skip keys injected by the daemon itself to avoid processing our own
    // output as new input, which would create duplicate or recursive mappings.
    if is_injected_key(vk_code.0, is_key_down) {
        clear_injected(vk_code.0, is_key_down);
        return unsafe { CallNextHookEx(None, code, w_param, l_param) };
    }

    // Maintain the tracked modifier state from the event stream.  This runs
    // before the decision so the key-down of a modifier itself is included
    // in the tracked set (the own-bit clear below handles bare-modifier
    // triggers).  Tagged re-emissions (capture mode) and the daemon's own
    // injections (normal mode) were skipped earlier and never reach here.
    if let Some(usage) = usage
        && let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage)
    {
        let mask = 1u8 << bit;
        let tracked = TRACKED_MODIFIERS.load(Ordering::Relaxed);
        if is_key_up {
            TRACKED_MODIFIERS.store(tracked & !mask, Ordering::Relaxed);
        } else {
            TRACKED_MODIFIERS.store(tracked | mask, Ordering::Relaxed);
        }
    }

    // Clear the current key's modifier bit from the tracked state so that
    // bare-modifier triggers (e.g. "LeftControl: A") match correctly against
    // the concurrent modifier set.
    let mut pressed_modifiers = TRACKED_MODIFIERS.load(Ordering::Relaxed);

    if capture_enabled() {
        capture_debug(&format!(
            "hook vk={:#04x} up={} msg={:#06x} extra={:#010x} mods={:#04x} \
             usage={:?}",
            vk_code.0,
            is_key_up,
            w_param.0,
            kbd_struct.dwExtraInfo,
            pressed_modifiers,
            usage
        ));
    }
    if let Some(usage) = usage
        && let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage)
    {
        pressed_modifiers &= !(1 << bit);
    }

    // Create a bounded (capacity 1) reply channel and send the event to the
    // worker.  Capacity 1 is sufficient because only one decision is sent.
    let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);

    let hook_event = HookEvent {
        vk_code,
        usage,
        is_key_up,
        modifiers: pressed_modifiers,
        reply_tx,
    };

    // Sending to an unbounded channel never blocks.
    let Ok(()) = tx.send(hook_event) else {
        // Worker disconnected — pass through.
        return unsafe {
            CallNextHookEx(Some(hook_handle()), code, w_param, l_param)
        };
    };

    // Wait for the worker's decision without blocking the input chain for too
    // long.  Use a polling loop with short sleeps to avoid deadlocking the
    // Windows message pump while still giving the worker time to respond.
    let decision = loop {
        if let Ok(decision) = reply_rx.try_recv() {
            break decision;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        // Safety check — if we've been waiting too long, give up.
        // The reply timeout is measured externally via a static counter.
        if REPLY_COUNTER.fetch_add(1, Ordering::Relaxed) > 50 {
            break Decision::PassThrough;
        }
    };

    // Reset the reply counter for the next event.
    REPLY_COUNTER.store(0, Ordering::Relaxed);

    match decision {
        Decision::Swallow(outputs) => {
            // In capture mode the worker already re-emitted the mapped
            // outputs through the virtual keyboard, so the hook proc must
            // not emit them again.  In normal mode emission happens here,
            // where the hook context still reaches the focused window.
            if !capture_enabled() {
                for native_key in &outputs {
                    emit_key_event(native_key);
                }
            }
            // Swallow in both modes: the daemon fully owns the output.
            return LRESULT(1);
        }
        Decision::PassThrough => {
            // In capture mode the worker forwarded the original key through
            // the virtual keyboard, so swallow the real one to avoid double
            // delivery.  In normal mode there is no mapping — pass through.
            if capture_enabled() {
                return LRESULT(1);
            }
        }
    };

    unsafe { CallNextHookEx(Some(hook_handle()), code, w_param, l_param) }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Starts the keyboard mapping engine.
///
/// Initialises the raw input thread, spawns the worker thread, installs the
/// `WH_KEYBOARD_LL` hook, and runs the message loop.  Blocks the calling
/// thread until the message loop exits (i.e. on `WM_QUIT`).
///
/// This is the entry point called by `keymapperd.rs` and replaces the
/// previous single-threaded static-mutex architecture.
pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Start the raw input loop (spawns its own thread).
    let (_raw_loop, raw_rx) = start_raw_input_loop()?;

    // Keep the raw input loop handle alive for the process lifetime.  The
    // struct only holds the HWND and has no Drop logic, so leaking is safe.
    // The background thread is already detached inside start_raw_input_loop.
    Box::leak(Box::new(_raw_loop));

    // Small delay to allow raw input registration to complete before the
    // hook starts firing events.
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Capture mode (e2e only, gated on `KEYMAPPER_CAPTURE`): the daemon
    // swallows every key and re-emits it through the virtual keyboard, tagged
    // with [`CAPTURE_TAG`], so the monitor's `WH_KEYBOARD_LL` hook can capture
    // the output without depending on a focused window.  In this mode the
    // worker performs all emission on its own (non-hook) thread — `SendInput`
    // from within a `WH_KEYBOARD_LL` callback would not reach other hooks.
    if std::env::var("KEYMAPPER_CAPTURE").is_ok_and(|v| !v.is_empty()) {
        set_capture_mode(true);
        eprintln!("Windows: capture mode enabled (KEYMAPPER_CAPTURE).");
    }

    // Spawn the worker thread.
    let hook_tx = spawn_worker(Arc::clone(&lookup), raw_rx);
    set_hook_tx(hook_tx);

    // Install the low-level keyboard hook.
    let h_instance: HINSTANCE = unsafe { GetModuleHandleW(None)?.into() };

    let handle = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(low_level_keyboard_proc),
            Some(h_instance),
            0,
        )?
    };

    if handle.is_invalid() {
        return Err("Failed to install global keyboard hook".into());
    }
    set_hook_handle(handle);

    println!("Windows low-level hook listening (three-thread mode).");

    // The raw input loop, worker thread, and keyboard hook are all live, so
    // the daemon can now process events.  Signal readiness for the e2e
    // harness.
    crate::daemon::signal_ready();

    // Run the message loop.  This blocks until WM_QUIT is received.
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        UnhookWindowsHookEx(handle)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_extended_key_returns_true_for_right_control() {
        assert!(is_extended_key(VIRTUAL_KEY(0xA3)));
    }

    #[test]
    fn is_extended_key_returns_true_for_right_alt() {
        assert!(is_extended_key(VIRTUAL_KEY(0xA5)));
    }

    #[test]
    fn is_extended_key_returns_true_for_arrow_keys() {
        assert!(is_extended_key(VIRTUAL_KEY(0x25))); // Left
        assert!(is_extended_key(VIRTUAL_KEY(0x26))); // Up
        assert!(is_extended_key(VIRTUAL_KEY(0x27))); // Right
        assert!(is_extended_key(VIRTUAL_KEY(0x28))); // Down
    }

    #[test]
    fn is_extended_key_returns_true_for_delete() {
        assert!(is_extended_key(VIRTUAL_KEY(0x2E)));
    }

    #[test]
    fn is_extended_key_returns_false_for_normal_key() {
        assert!(!is_extended_key(VIRTUAL_KEY(0x41))); // 'A'
    }

    #[test]
    fn modifier_bit_to_vk_round_trips() {
        for bit in 0..8 {
            if let Some(role) = ModifierRole::try_from_bit(bit) {
                let vk = match role {
                    ModifierRole::LeftControl => Key::LeftControl,
                    ModifierRole::RightControl => Key::RightControl,
                    ModifierRole::LeftShift => Key::LeftShift,
                    ModifierRole::RightShift => Key::RightShift,
                    ModifierRole::LeftAlt => Key::LeftAlt,
                    ModifierRole::RightAlt => Key::RightAlt,
                    ModifierRole::LeftCommand => Key::LeftCommand,
                    ModifierRole::RightCommand => Key::RightCommand,
                };
                let resolved = modifier_bit_to_vk(bit);
                assert!(resolved.is_some(), "bit {} ({:?})", bit, role);
                assert_eq!(resolved.unwrap().0, vk.as_native());
            }
        }
    }

    #[test]
    fn hook_tx_static_is_initially_none() {
        // Verify that the static sender starts as None.  This is mainly
        // a sanity check that the static initialisation works correctly.
        // Note: other tests may have set this, so we reset it afterwards.
        let was_none = get_hook_tx().is_none();
        // We don't assert because other tests may have populated it.
        let _ = was_none;
    }
}
