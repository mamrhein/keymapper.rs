// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows keyboard mapping via a two-thread architecture.
//!
//! The hook thread installs a `WH_KEYBOARD_LL` hook and runs the message
//! loop.  On each key event the hook proc performs the mapping decision
//! itself — matching the event against the raw input buffer for device
//! identification and asking the shared [`MappingEngine`] — and either
//! emits the mapped output directly via `SendInput` and swallows the key,
//! or passes it through.  The engine owns all bookkeeping (pressed and
//! swallowed keys, forwarded/consumed modifier masks, held output
//! modifiers): when a trigger fires while the user still holds a forwarded
//! modifier key, that modifier is released first (a tagged key-up), so the
//! emitted output is a clean tap instead of riding on the held modifier,
//! and the modifier's physical release is swallowed.  An output whose base
//! is itself a modifier key (e.g. `CapsLock -> LeftControl`) is held down
//! on the output side until the physical key-up, so the remapped modifier
//! stays active for subsequent key presses.
//!
//! The mapped output is emitted directly in the hook callback: a `SendInput`
//! issued from within a `WH_KEYBOARD_LL` callback reaches other hooks and the
//! target window, so the previous design (worker thread, one-shot reply
//! channel, deferred emission) is no longer load-bearing for keyboard events.
//!
//! Thread layout:
//!
//! 1. **Hook thread** — Installs the \`WH_KEYBOARD_LL\` hook and runs a
//!    blocking \`GetMessageW\` loop.  The loop dispatches the raw input
//!    window's \`WM_INPUT\` (the window is owned by this thread) and drains
//!    the emission queue on a \`WM_APP\` wake (fed only by standalone
//!    consumer events).  Decides and emits in-callback.
//! 2. **Raw worker thread** — Consumes the raw input channel, maintains the
//!    device-identification buffer, and processes standalone Consumer Control
//!    events, which never reach the hook.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use parking_lot::RwLock;
// The drain wake post is only compiled into non-test builds (see
// `queue_emission`); unit tests never queue an emission.
#[cfg(not(test))]
use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
use windows::Win32::{
    Foundation::{GetLastError, HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::{
        LibraryLoader::GetModuleHandleW,
        Threading::GetCurrentThreadId,
    },
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
            KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC,
            MapVirtualKeyW, SendInput, VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK,
            KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW, TranslateMessage,
            UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN,
            WM_SYSKEYDOWN,
        },
    },
};

use super::{
    INJECTED_TAG,
    device_match::{device_cache, match_usage},
    key::{Key, hid_to_vk},
    raw_input::start_raw_input_loop,
    raw_worker::spawn_raw_worker,
};
use crate::{
    common::{
        hid_usage::HidUsage, keyboard::KeyboardSpecifier,
        modifier::ModifierRole,
    },
    daemon::{
        engine::{Decision, MappingEngine, output_held_mask},
        mapping_cache::NativeKey,
        state::Lookup,
    },
};

// ---------------------------------------------------------------------------
// Static state for the hook procedure
// ---------------------------------------------------------------------------

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

/// The engine's key identity: (scan code, extended flag), so keys sharing
/// a virtual-key code (e.g. the two Shifts) stay distinct.
type EngineKey = (u16, bool);

/// The unified mapping engine, shared with the hook proc.
///
/// The hook proc is a `extern "system"` fn and cannot capture locals, so
/// the engine is parked in a process-wide static that is set once from
/// [`start_mapping`] before the hook is installed.  The hook proc treats a
/// missing engine as "pass through" so a key event can never stall the
/// input chain while the engine is not up.  The low-level hook is
/// serialized, so a single instance serves the whole session; the mutex
/// only keeps each decision atomic (the tagged self-injections re-entering
/// the hook return before the lock is taken).
static ENGINE: std::sync::OnceLock<
    parking_lot::Mutex<MappingEngine<EngineKey>>,
> = std::sync::OnceLock::new();

fn set_engine(engine: MappingEngine<EngineKey>) {
    let _ = ENGINE.set(parking_lot::Mutex::new(engine));
}

fn engine() -> Option<&'static parking_lot::Mutex<MappingEngine<EngineKey>>> {
    ENGINE.get()
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

/// The hardware scan code and extended-key flag for *vk*, as reported by
/// `MapVirtualKeyW` (whose high scan bit marks extended keys).  A zero scan
/// code means the VK has no hardware scan code (e.g. some multimedia keys);
/// such events are emitted with `wScan: 0` as before.
fn scan_code_and_extended(vk: VIRTUAL_KEY) -> (u16, bool) {
    let scan = unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) };
    ((scan & 0xFF) as u16, scan & 0x100 != 0)
}

fn simulate_key_event(vk: VIRTUAL_KEY, is_key_up: bool) {
    let (scan, extended) = scan_code_and_extended(vk);
    let mut flags: u32 = if is_key_up { KEYEVENTF_KEYUP.0 } else { 0 };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY.0;
    }

    // Stamp the daemon tag so the hook proc recognizes the event as our own
    // injection and passes it through without re-mapping.
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(flags),
                time: 0,
                dwExtraInfo: INJECTED_TAG,
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

/// Release the given modifier bits on the output side (tagged key-ups), in
/// ascending bit order.
///
/// These are the releases that make a fired trigger's output a clean tap
/// (the trigger's forwarded or held modifiers) and the physical key-up of
/// a remapped modifier (its held output bits).  The 1 ms pacing matches
/// [`emit_key_event`].
fn release_modifiers(mask: u8) {
    for bit in 0..8 {
        if mask & (1 << bit) != 0
            && let Some(vk) = modifier_bit_to_vk(bit)
        {
            simulate_key_event(vk, true);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

/// Press the modifier bits of a modifier-key output, keeping them held on
/// the output side until the matching release.
///
/// Unlike [`emit_key_event`] (which emits a self-contained tap), the keys
/// stay down until the physical key-up — which is what makes a remapped
/// modifier usable for the key presses that follow it.  The output's other
/// modifier bits are pressed first (ascending), the base last, mirroring
/// the tap's press order.
fn hold_modifier_output(native_key: &NativeKey) {
    let base_bit = HidUsage::hid_usage_to_modifier_bit(native_key.usage);

    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1
            && base_bit != Some(bit)
            && let Some(vk) = modifier_bit_to_vk(bit)
        {
            simulate_key_event(vk, false);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    if let Some(bit) = base_bit
        && let Some(vk) = modifier_bit_to_vk(bit)
    {
        simulate_key_event(vk, false);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

// ---------------------------------------------------------------------------
// Deferred emission
// ---------------------------------------------------------------------------

/// Outputs queued for the main message loop to send via `SendInput`.
///
/// Only standalone consumer events are queued (see `raw_worker`): their
/// emission originates on the raw input thread, where a `SendInput` could
/// race a keyboard hook chain in progress and be dropped by the input
/// system.  Mapped keyboard outputs are emitted in-callback by the hook
/// proc directly, so they never pass through this queue.
static PENDING_EMISSIONS: parking_lot::Mutex<Vec<Vec<NativeKey>>> =
    parking_lot::Mutex::new(Vec::new());

/// The main message loop's thread id, recorded in `start_mapping` so the
/// raw input thread can post the drain wake message.
static MAIN_THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Records the main loop's thread id for the drain wake post.
fn set_main_thread_id(tid: u32) {
    MAIN_THREAD_ID.store(tid, Ordering::Relaxed);
}

/// Queue a set of mapped outputs for emission by the main message loop and
/// wake the loop so it drains the queue.
///
/// The raw input thread calls this for standalone consumer events.  A
/// swallowed hook event produces no message of its own, so the posted
/// `WM_APP` message is what makes the blocked `MsgWaitForMultipleObjects`
/// return and the loop body (the drain) run.  If the wake is posted while a
/// hook chain is still in progress it simply waits in the queue: the loop
/// returns from the wait only once the chain has completed, so the drain
/// always runs with the chain idle.
#[cfg(not(test))]
pub(super) fn queue_emission(outputs: Vec<NativeKey>) {
    PENDING_EMISSIONS.lock().push(outputs);
    let tid = MAIN_THREAD_ID.load(Ordering::Relaxed);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_APP, WPARAM(0), LPARAM(0));
        }
    }
}

/// Emit all queued outputs via `SendInput`.  The main message loop calls
/// this from the loop body, after the hook chain has completed, so the
/// `SendInput` is issued with the input queue idle.
fn drain_and_emit_emissions() {
    let pending = std::mem::take(&mut *PENDING_EMISSIONS.lock());
    for outputs in pending {
        for native_key in &outputs {
            emit_key_event(native_key);
        }
    }
}

// ---------------------------------------------------------------------------
// Low-level keyboard hook procedure
// ---------------------------------------------------------------------------

/// Whether per-event hook diagnostics are enabled (set `KEYMAPPER_HOOK_LOG`
/// to any value).  Read once and cached so the hot path never calls
/// `getenv`.  Used by the e2e harness to confirm the hook fires and to see
/// the decision made for each key-down.
fn hook_log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("KEYMAPPER_HOOK_LOG").is_some())
}

extern "system" fn low_level_keyboard_proc(
    code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, w_param, l_param) };
    }

    let kbd_struct = unsafe { &*(l_param.0 as *const KBDLLHOOKSTRUCT) };
    let vk_code = VIRTUAL_KEY(kbd_struct.vkCode as u16);
    let vk = vk_code.0;

    // Every key the daemon injects through `SendInput` is stamped with
    // [`INJECTED_TAG`].  Let those flow on without re-mapping them, so the
    // target window (or the e2e monitor's hook) receives them.  Matching on
    // the tag is exact, so a physical press of the same key can never be
    // swallowed as one of our own injections.
    if kbd_struct.dwExtraInfo == INJECTED_TAG {
        return unsafe {
            CallNextHookEx(Some(hook_handle()), code, w_param, l_param)
        };
    }

    let is_key_up = !matches!(w_param.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);

    // Derive the HID identity of the key — the lookup key space of the
    // compiled rules.  `None` for virtual-key codes without a `HidUsage`
    // (e.g. Print Screen); such keys always pass through.
    let Some(usage) = Key::from_native(vk_code.0).map(Key::to_hid_usage)
    else {
        if !is_key_up && hook_log_enabled() {
            eprintln!("hook: down vk={vk:#04x} -> no usage (pass)");
        }
        return unsafe {
            CallNextHookEx(Some(hook_handle()), code, w_param, l_param)
        };
    };

    // Identify the source keyboard non-blockingly.  Raw input and the hook
    // do not deliver in a guaranteed order, so retry for a few milliseconds
    // — long enough for the raw event of this same press to arrive in the
    // common case, short enough to keep the hook callback well inside
    // Windows' low-level-hook timeout.  A press that never matches degrades
    // to a lookup without device identification (device-filtered rules
    // simply do not fire for it).
    let device_path = match_usage_with_retry(usage)
        .and_then(|handle_ptr| device_cache().get_or_resolve(handle_ptr));

    // The engine decides the event's fate from its own bookkeeping
    // (pressed/swallowed keys, forwarded/consumed modifier masks, held
    // output modifiers); this layer only executes the decision.  The lock
    // is held for the decision alone — never across the emission below,
    // whose `SendInput` re-enters this hook for the tagged events.
    let Some(engine) = engine() else {
        // The engine is not up (or is shutting down); never block the
        // input chain on that.
        if !is_key_up && hook_log_enabled() {
            eprintln!(
                "hook: down vk={vk:#04x} usage={} -> no engine (pass)",
                usage.as_str()
            );
        }
        return unsafe {
            CallNextHookEx(Some(hook_handle()), code, w_param, l_param)
        };
    };
    let decision = engine.lock().decide(
        (kbd_struct.scanCode as u16, kbd_struct.flags.0 & 1 != 0),
        usage,
        is_key_up,
        device_path.as_deref(),
        true,
    );

    // Diagnostic (enabled with `KEYMAPPER_HOOK_LOG`): log each key-down and
    // its decision so a CI failure shows whether the hook fires, what usage
    // it computes, and whether the lookup finds a rule.  Key-ups are omitted
    // to keep the log readable.
    if !is_key_up && hook_log_enabled() {
        let summary = match &decision {
            Decision::Pass => "pass".to_string(),
            Decision::Emit { outputs, .. } => {
                format!("emit({})", outputs.len())
            }
            Decision::Swallow { .. } => "swallow".to_string(),
            Decision::ConsumedRelease => "consumed-release".to_string(),
        };
        eprintln!(
            "hook: down vk={vk:#04x} scan={:02x} ext={} usage={} -> {}",
            kbd_struct.scanCode,
            kbd_struct.flags.0 & 1 != 0,
            usage.as_str(),
            summary,
        );
    }

    // A `SendInput` issued from within a `WH_KEYBOARD_LL` callback reaches
    // other hooks and the target window (the capture-mode e2e tests capture
    // the tagged re-emission from a separate process's hook), so the mapped
    // output is emitted directly in the callback.
    match decision {
        // Unmapped (or the repeat of an unmapped key): let the event through.
        Decision::Pass => {
            unsafe {
                CallNextHookEx(Some(hook_handle()), code, w_param, l_param)
            }
        }
        // Mapped: release the trigger's modifiers first (clean tap), then
        // emit the outputs.  An output whose base is itself a modifier key
        // is held down (not tapped) so the remapped modifier stays active
        // for subsequent key presses; the matching release is emitted when
        // the physical key-up arrives.
        Decision::Emit { release, outputs } => {
            if release != 0 {
                release_modifiers(release);
            }
            for native_key in &outputs {
                if output_held_mask(native_key).is_some() {
                    hold_modifier_output(native_key);
                } else {
                    emit_key_event(native_key);
                }
            }
            LRESULT(1)
        }
        // A mapped key-up: swallow the event and, for a remapped modifier
        // key, release the output bits that have been held since the key-down.
        Decision::Swallow { release } => {
            if release != 0 {
                release_modifiers(release);
            }
            LRESULT(1)
        }
        // A consumed modifier release: the synthetic key-up was already sent
        // when the trigger fired, so swallow the physical release.
        Decision::ConsumedRelease => LRESULT(1),
    }
}

/// Match a raw input event for *usage* against the device-identification
/// buffer, retrying for a few milliseconds to absorb the non-deterministic
/// delivery order between the raw input and hook streams.
///
/// Returns the device handle pointer of the matching event, or `None` when
/// no raw input event arrives within the budget (the caller then falls back
/// to a lookup without device identification).
fn match_usage_with_retry(usage: HidUsage) -> Option<usize> {
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(3);
    loop {
        if let Some(handle_ptr) = match_usage(usage) {
            return Some(handle_ptr);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Starts the keyboard mapping engine.
///
/// Initialises the raw input loop, spawns the raw worker thread, installs
/// the `WH_KEYBOARD_LL` hook, and runs the message loop.  Blocks the
/// calling thread until the message loop exits (i.e. on `WM_QUIT`).
///
/// This is the entry point called by `keymapperd.rs`.
///
/// `keyboard_filter` is accepted for signature uniformity with the other
/// platforms but is a no-op on Windows: capture is a session-global
/// `WH_KEYBOARD_LL` hook, and applying the filter per device (via raw input
/// device ids) is a feature deliberately out of scope for this phase.
pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    #[allow(unused_variables)] keyboard_filter: Option<Vec<KeyboardSpecifier>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Start the raw input loop (creates the message-only window on this
    // thread, which owns its `WM_INPUT` messages).
    let (_raw_loop, raw_rx) = start_raw_input_loop()?;

    // Keep the raw input loop handle alive for the process lifetime.  The
    // struct only holds the HWND and has no Drop logic, so leaking is safe.
    Box::leak(Box::new(_raw_loop));

    // Record this thread's id so the raw worker can post the drain wake
    // message to the message loop (done before the raw worker starts, so no
    // emission can be queued before the id is recorded).
    set_main_thread_id(unsafe { GetCurrentThreadId() });

    // Park the engine where the hook proc can find it, then spawn the raw
    // worker (which consumes the raw input channel).
    set_engine(MappingEngine::new(Arc::clone(&lookup)));
    spawn_raw_worker(lookup, raw_rx);

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

    println!("Windows low-level hook listening (two-thread mode).");

    if hook_log_enabled() {
        eprintln!("hook: installed, pumping on main thread");
    }

    // Run the message loop until WM_QUIT.  A `WH_KEYBOARD_LL` callback is
    // only invoked while the installing thread pumps messages through the
    // blocking `GetMessageW`; a non-blocking `PeekMessageW` drain (the
    // previous approach) never triggered the callback, so every key passed
    // through without being remapped.  The only queueing emission is from
    // standalone consumer events: the raw worker posts a `WM_APP` wake
    // through `queue_emission`, which we intercept here to drain the pending
    // consumer outputs.  All other messages — including the raw input
    // window's `WM_INPUT`, which is owned by this thread — are translated and
    // dispatched normally.
    unsafe {
        let mut msg = MSG::default();
        let mut logged_first_message = false;
        loop {
            let got_message = GetMessageW(&mut msg, None, 0, 0);
            // `GetMessageW` returns FALSE on WM_QUIT (and on error).
            if !got_message.as_bool() {
                // Distinguish WM_QUIT (0) from an error (-1): the loop exit
                // ends the daemon, so a CI log must show why it happened.
                if hook_log_enabled() {
                    let last_error = GetLastError();
                    eprintln!(
                        "hook: message loop exited, got={}, last_error={}",
                        got_message.0, last_error.0
                    );
                }
                break;
            }
            if !logged_first_message && hook_log_enabled() {
                logged_first_message = true;
                eprintln!("hook: first message 0x{:08x}", msg.message);
            }
            if msg.message == WM_APP {
                drain_and_emit_emissions();
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
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
    fn scan_code_and_extended_marks_extended_keys() {
        // Delete and the right-hand modifiers are extended keys; their scan
        // codes carry the high bit regardless of keyboard layout.
        for vk in [0x2D, 0xA3, 0xA5] {
            let (_, extended) = scan_code_and_extended(VIRTUAL_KEY(vk));
            assert!(extended, "VK {vk:#04x} should be extended");
        }
    }

    #[test]
    fn scan_code_and_extended_marks_normal_keys() {
        // A regular letter is not extended and has a non-zero scan code.
        let (scan, extended) = scan_code_and_extended(VIRTUAL_KEY(0x41));
        assert!(!extended);
        assert_ne!(scan, 0);
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
}
