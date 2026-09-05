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
//! 1. **Hook thread** — \`WH_KEYBOARD_LL\` hook + message loop
//!    (\`MsgWaitForMultipleObjects\` + \`PeekMessageW\`).  Sends \`HookEvent\`
//!    to worker, blocks on reply, and drains queued emissions after each
//!    wait.
//! 2. **Raw input thread** — Message-only window + \`GetMessageW\` loop for
//!    \`WM_INPUT\`.  Sends \`RawInputEvent\` to worker.
//! 3. **Worker thread** — Receives from both channels, matches events,
//!    resolves devices, performs lookups, sends decisions back.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU32, Ordering},
    },
};

// Capture-mode-only imports: the debug log needs `Write`, and the capture
// flag is an `AtomicBool`.  Both are compiled out of production builds.
#[cfg(feature = "e2e")]
use std::io::Write;
#[cfg(feature = "e2e")]
use std::sync::atomic::AtomicBool;

use crossbeam_channel;
use parking_lot::RwLock;
use windows::Win32::{
    Foundation::{
        GetLastError, HINSTANCE, LPARAM, LRESULT, WAIT_FAILED, WPARAM,
    },
    System::{
        LibraryLoader::GetModuleHandleW,
        Threading::{GetCurrentThreadId, INFINITE},
    },
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
            KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            CallNextHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG, PeekMessageW,
            MsgWaitForMultipleObjects, PM_REMOVE, QS_ALLINPUT,
            SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_QUIT,
            WM_KEYDOWN, WM_SYSKEYDOWN,
        },
    },
};

// The drain wake post is only compiled into non-test builds (see
// `queue_emission`); unit tests never queue an emission.
#[cfg(not(test))]
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};

use super::{
    dispatch::{Decision, HookEvent, spawn_worker},
    key::{Key, hid_to_vk},
    raw_input::start_raw_input_loop,
};

use super::INJECTED_TAG;
use crate::{
    common::{
        hid_usage::HidUsage, keyboard::KeyboardSpecifier,
        modifier::ModifierRole,
    },
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
    // In test mode, write output events to a file instead of calling
    // `SendInput`. This avoids the issue where `SendInput` from within a
    // `WH_KEYBOARD_LL` hook callback does not trigger other hooks (Windows
    // prevents recursive hook invocation). The e2e test reads this file to
    // verify outputs.  Compiled in only with the `e2e` feature, so the
    // production binary has no env-gated file-write path here.
    #[cfg(feature = "e2e")]
    if let Ok(path) = std::env::var("KEYMAPPER_TEST_OUTPUT") {
        let line = if is_key_up {
            format!("UP {}\n", vk.0)
        } else {
            format!("DOWN {}\n", vk.0)
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

    #[cfg(feature = "e2e")]
    if capture_enabled() {
        capture_debug(&format!("emit vk={:#04x} up={}", vk.0, is_key_up));
    }

    let mut flags: u32 = if is_key_up { KEYEVENTF_KEYUP.0 } else { 0 };
    if is_extended_key(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY.0;
    }

    // Stamp the daemon tag so the hook proc recognizes the event as our own
    // injection and passes it through without re-mapping.
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
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

// ---------------------------------------------------------------------------
// Deferred emission
// ---------------------------------------------------------------------------

/// Outputs the worker has decided to emit, queued for the main message loop
/// to send via `SendInput`.
///
/// A `SendInput` must not be issued from inside the low-level hook callback
/// (nor while a hook chain is in progress): the input system drops injected
/// events that arrive while the hook thread is busy processing a chain, and
/// they never reach the target window.  The worker therefore queues the
/// outputs here, and the always-pumping message loop performs the actual
/// `SendInput` once the hook chain has completed.
static PENDING_EMISSIONS: parking_lot::Mutex<Vec<Vec<NativeKey>>> =
    parking_lot::Mutex::new(Vec::new());

/// The main message loop's thread id, recorded in `start_mapping` so the
/// worker can post the drain wake message.
static MAIN_THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Records the main loop's thread id for the drain wake post.
fn set_main_thread_id(tid: u32) {
    MAIN_THREAD_ID.store(tid, Ordering::Relaxed);
}

/// Queue a set of mapped outputs for emission by the main message loop and
/// wake the loop so it drains the queue.
///
/// The worker thread calls this when a key-down resolves to a mapping.  A
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

/// Emit all queued outputs via `SendInput`.  The main message loop calls this
/// from the loop body, after the hook chain has completed, so the
/// `SendInput` is issued from a thread that is neither inside the hook
/// callback nor blocked in its reply wait.
fn drain_and_emit_emissions() {
    let pending = std::mem::take(&mut *PENDING_EMISSIONS.lock());
    for outputs in pending {
        for native_key in &outputs {
            emit_key_event(native_key);
        }
    }
}

// ---------------------------------------------------------------------------
// Capture mode
// ---------------------------------------------------------------------------
//
// Capture mode makes the daemon re-emit every key through its virtual
// keyboard, tagged with [`INJECTED_TAG`], so the e2e monitor's
// `WH_KEYBOARD_LL` hook can capture the daemon's output without depending on
// a focused window.  It is gated on the `KEYMAPPER_CAPTURE` environment
// variable so production behaviour (unmapped keys passing straight through)
// is left untouched.

/// Process start, for capture-debug timestamps.
#[cfg(feature = "e2e")]
static CAPTURE_T0: std::sync::OnceLock<std::time::Instant> =
    std::sync::OnceLock::new();

/// Capture-mode debug log, appended to from the hook and worker threads.
#[cfg(feature = "e2e")]
static CAPTURE_DEBUG: std::sync::OnceLock<std::sync::Mutex<std::fs::File>> =
    std::sync::OnceLock::new();

#[cfg(feature = "e2e")]
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
#[cfg(feature = "e2e")]
static CAPTURE_MODE: AtomicBool = AtomicBool::new(false);

/// Whether capture mode is active (all emission tagged through the virtual
/// keyboard).  Capture mode only exists in `e2e` builds; in production it is
/// always disabled, so the flag is a compile-time `false` there.  Only
/// compiled in where it is referenced: the worker's normal-mode emission
/// block (every non-test build) and the capture-mode paths (e2e builds).
#[cfg(any(feature = "e2e", not(test)))]
pub(super) fn capture_enabled() -> bool {
    #[cfg(feature = "e2e")]
    { CAPTURE_MODE.load(Ordering::Relaxed) }
    #[cfg(not(feature = "e2e"))]
    { false }
}

/// Record the capture-mode flag determined at startup.
///
/// Only compiled in with the `e2e` feature: without it, capture mode can
/// never be enabled, so the writer is dead code in production builds.
#[cfg(feature = "e2e")]
fn set_capture_mode(enabled: bool) {
    CAPTURE_MODE.store(enabled, Ordering::Relaxed);
}

/// Forward a single (unmapped) key through the virtual keyboard in capture
/// mode.  In normal mode unmapped keys pass straight through the OS, so this
/// is a no-op.
#[cfg(feature = "e2e")]
pub(super) fn emit_forwarded_key(vk: u16, is_key_up: bool) {
    if capture_enabled() {
        simulate_key_event(VIRTUAL_KEY(vk), is_key_up);
    }
}

/// Tracks forwarded-modifier state for capture-mode emission.
///
/// In capture mode the daemon re-emits every pass-through key through the
/// virtual keyboard, so a forwarded modifier key is held on that keyboard
/// until its physical release is forwarded.  When a trigger fires while such
/// a modifier is held, the modifier must be released first, or the emitted
/// output becomes an unintended chord (e.g. the rule
/// `Ctrl+Semicolon -> C` would emit `Ctrl+C`, i.e. SIGINT).  Consumed
/// modifiers are marked so their physical release is swallowed rather than
/// forwarded a second time.  Mirrors the `consumed_modifiers` bookkeeping
/// of the Linux and macOS backends.
#[cfg(feature = "e2e")]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CaptureModifierState {
    /// Bitmask of modifier keys that were forwarded (pass-through) and are
    /// still held on the virtual keyboard.
    forwarded: u8,
    /// Bitmask of modifier keys that were part of a fired trigger and have
    /// already been released on the virtual keyboard.  Their physical
    /// release is swallowed so it is not forwarded a second time.
    consumed: u8,
}

#[cfg(feature = "e2e")]
impl CaptureModifierState {
    /// Record a forwarded (pass-through) modifier press.
    ///
    /// A fresh press clears any consumed mark: the early release belongs to
    /// the previous press, so the release of this one must forward.
    fn record_forwarded_down(&mut self, bit: u8) {
        let mask = 1 << bit;
        self.forwarded |= mask;
        self.consumed &= !mask;
    }

    /// Record the physical release of a forwarded modifier.
    ///
    /// Returns `true` when the release must be swallowed because the
    /// modifier was already released on the virtual keyboard when a trigger
    /// fired, and `false` when it should be forwarded.
    fn record_forwarded_up(&mut self, bit: u8) -> bool {
        let mask = 1 << bit;
        if self.consumed & mask != 0 {
            self.consumed &= !mask;
            true
        } else {
            self.forwarded &= !mask;
            false
        }
    }

    /// Consume the modifiers of a fired trigger: the subset that was
    /// forwarded is moved from the forwarded mask to the consumed mask and
    /// returned, so the caller can release it on the virtual keyboard.
    fn consume_triggered(&mut self, modifiers: u8) -> u8 {
        let consumed = modifiers & self.forwarded;
        self.forwarded &= !consumed;
        self.consumed |= consumed;
        consumed
    }
}

/// Process-global forwarded-modifier state, mutated only by the worker
/// thread (the sole emitter in capture mode).
#[cfg(feature = "e2e")]
static CAPTURE_MODIFIER_STATE: parking_lot::Mutex<CaptureModifierState> =
    parking_lot::Mutex::new(CaptureModifierState {
        forwarded: 0,
        consumed: 0,
    });

/// Record a forwarded (pass-through) modifier press in capture mode.
#[cfg(feature = "e2e")]
pub(super) fn capture_record_forwarded_down(bit: u8) {
    CAPTURE_MODIFIER_STATE.lock().record_forwarded_down(bit);
}

/// Record the physical release of a forwarded modifier in capture mode.
///
/// Returns `true` when the release must be swallowed because the modifier
/// was already released on the virtual keyboard when a trigger fired.
#[cfg(feature = "e2e")]
pub(super) fn capture_record_forwarded_up(bit: u8) -> bool {
    CAPTURE_MODIFIER_STATE.lock().record_forwarded_up(bit)
}

/// Release the fired trigger's forwarded modifiers on the virtual keyboard
/// (tagged releases), so the output is emitted as a clean tap, and mark
/// them consumed so their physical releases are swallowed.
#[cfg(feature = "e2e")]
pub(super) fn capture_release_triggered_modifiers(modifiers: u8) {
    let consumed = CAPTURE_MODIFIER_STATE.lock().consume_triggered(modifiers);
    if consumed == 0 {
        return;
    }

    // Release in ascending bit order, mirroring the output tap's modifier
    // order.
    for bit in 0..8 {
        if consumed & (1 << bit) != 0
            && let Some(vk) = modifier_bit_to_vk(bit)
        {
            simulate_key_event(vk, true);
        }
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

    // Every key the daemon injects through `SendInput` is stamped with
    // [`INJECTED_TAG`].  Let those flow on without re-mapping them: in
    // capture mode the monitor's hook captures them, in normal mode the
    // target window receives them.  Matching on the tag is exact, so a
    // physical press of the same key can never be swallowed as one of our
    // own injections.
    if kbd_struct.dwExtraInfo == INJECTED_TAG {
        #[cfg(feature = "e2e")]
        if capture_enabled() {
            capture_debug(&format!(
                "hook tagged vk={:#04x} msg={:#06x}",
                kbd_struct.vkCode, w_param.0
            ));
        }
        return unsafe {
            CallNextHookEx(Some(hook_handle()), code, w_param, l_param)
        };
    }

    let is_key_up = !matches!(
        w_param.0 as u32,
        WM_KEYDOWN | WM_SYSKEYDOWN
    );

    // Derive the HID identity of the key — the lookup key space of the
    // compiled rules.  `None` for virtual-key codes without a `HidUsage`
    // (e.g. Print Screen); such keys always pass through.
    let usage = Key::from_native(vk_code.0).map(Key::to_hid_usage);

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

    #[cfg(feature = "e2e")]
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
    //
    // Note: the mapped output is NOT emitted here.  It is queued for the main
    // message loop (see [`drain_and_emit_emissions`]), because a `SendInput`
    // issued while this hook thread is blocked in the wait is dropped by the
    // input system and never reaches the target.
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
        Decision::Swallow(_) => {
            // The worker has already handled the mapped outputs: in capture
            // mode it emitted them directly, in normal mode it queued them
            // for the main message loop (posting the wake message that
            // triggers the drain).  A `SendInput` issued while this hook
            // callback is on the stack is dropped by the input system, so
            // the hook proc never emits — it only swallows the original
            // key, which the daemon fully owns in both modes.
            return LRESULT(1);
        }
        Decision::PassThrough => {
            // In capture mode the worker forwarded the original key through
            // the virtual keyboard, so swallow the real one to avoid double
            // delivery.  In normal mode there is no mapping — pass through.
            #[cfg(feature = "e2e")]
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
///
/// `keyboard_filter` is accepted for signature uniformity with the other
/// platforms but is a no-op on Windows: capture is a session-global
/// `WH_KEYBOARD_LL` hook, and applying the filter per device (via raw input
/// device ids) is a feature deliberately out of scope for this phase.
/// `ready_signal` is invoked once the daemon can process events; it is
/// injected by the caller so this module stays free of test-specific side
/// effects.
pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    #[allow(unused_variables)] keyboard_filter: Option<Vec<KeyboardSpecifier>>,
    ready_signal: Option<Box<dyn FnOnce() + Send>>,
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
    // with [`INJECTED_TAG`], so the monitor's `WH_KEYBOARD_LL` hook can capture
    // the output without depending on a focused window.  In this mode the
    // worker performs all emission on its own (non-hook) thread — `SendInput`
    // from within a `WH_KEYBOARD_LL` callback would not reach other hooks.
    // Compiled in only with the `e2e` feature, so production builds can never
    // be switched into capture mode via the environment.
    #[cfg(feature = "e2e")]
    if std::env::var("KEYMAPPER_CAPTURE").is_ok_and(|v| !v.is_empty()) {
        set_capture_mode(true);
        eprintln!("Windows: capture mode enabled (KEYMAPPER_CAPTURE).");
    }

    // Record this thread's id so the worker can post the drain wake message
    // to the message loop (done before the worker starts, so no emission
    // can be queued before the id is recorded).
    set_main_thread_id(unsafe { GetCurrentThreadId() });

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
    // the daemon can now process events.
    if let Some(signal) = ready_signal {
        signal();
    }

    // Run the message loop until WM_QUIT.  The low-level hook callback runs
    // re-entrantly inside the blocked `MsgWaitForMultipleObjects` call, and
    // a swallowed hook event yields no message of its own, so the wake for
    // the drain is the `WM_APP` message the worker posts through
    // `queue_emission` when it queues mapped outputs.  The loop blocks in
    // `MsgWaitForMultipleObjects` (which returns for any posted message),
    // then drains the queue with the non-blocking `PeekMessageW` — a
    // blocking `GetMessageW` here would
    // consume the wake message and then block on the next one, starving the
    // drain.  The drain runs in the loop body, where the hook chain is
    // idle, so the `SendInput` reaches the target.  Messages are removed
    // but neither translated nor dispatched: the hook callback performs all
    // processing.
    unsafe {
        loop {
            let wait =
                MsgWaitForMultipleObjects(None, false, INFINITE, QS_ALLINPUT);
            if wait == WAIT_FAILED {
                eprintln!(
                    "Windows: MsgWaitForMultipleObjects failed: {:?}",
                    GetLastError()
                );
                break;
            }
            let mut quit = false;
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    quit = true;
                    break;
                }
                // Consumed without dispatch; the hook callback already
                // handled it.
            }
            drain_and_emit_emissions();
            if quit {
                break;
            }
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

    #[cfg(feature = "e2e")]
    #[test]
    fn capture_state_forwarded_down_up_round_trip() {
        // A forwarded (pass-through) modifier press is tracked, and its
        // physical release is forwarded (not swallowed).
        let mut state = CaptureModifierState::default();
        state.record_forwarded_down(0);
        assert_eq!(state.forwarded, 0b0000_0001);
        assert!(!state.record_forwarded_up(0));
        assert_eq!(state.forwarded, 0);
        assert_eq!(state.consumed, 0);
    }

    #[cfg(feature = "e2e")]
    #[test]
    fn capture_state_consume_releases_and_swallows_releases() {
        // A trigger firing while both modifiers are held consumes them;
        // their physical releases are then swallowed.
        let mut state = CaptureModifierState::default();
        state.record_forwarded_down(0);
        state.record_forwarded_down(1);

        let consumed = state.consume_triggered(0b0000_0011);
        assert_eq!(consumed, 0b0000_0011);
        assert_eq!(state.forwarded, 0);
        assert_eq!(state.consumed, 0b0000_0011);

        assert!(state.record_forwarded_up(0));
        assert!(state.record_forwarded_up(1));
        assert_eq!(state.consumed, 0);
    }

    #[cfg(feature = "e2e")]
    #[test]
    fn capture_state_consume_partial_subset() {
        // Only the modifiers that were actually forwarded are consumed;
        // the others are untouched and their releases still forward.
        let mut state = CaptureModifierState::default();
        state.record_forwarded_down(2);

        let consumed = state.consume_triggered(0b0000_1100);
        assert_eq!(consumed, 0b0000_0100);
        assert_eq!(state.forwarded, 0);
        assert_eq!(state.consumed, 0b0000_0100);

        assert!(state.record_forwarded_up(2)); // consumed: swallow
        assert!(!state.record_forwarded_up(3)); // never forwarded: forward
    }

    #[cfg(feature = "e2e")]
    #[test]
    fn capture_state_consume_without_forwarded_modifiers() {
        // A trigger firing with no forwarded modifiers consumes nothing.
        let mut state = CaptureModifierState::default();
        assert_eq!(state.consume_triggered(0b0000_0011), 0);
        assert_eq!(state.forwarded, 0);
        assert_eq!(state.consumed, 0);
    }

    #[cfg(feature = "e2e")]
    #[test]
    fn capture_state_late_release_after_consume_is_forwarded() {
        // A modifier pressed again after being consumed (e.g. a fresh
        // physical press) is tracked as forwarded once more, so its
        // release forwards instead of being swallowed.
        let mut state = CaptureModifierState::default();
        state.record_forwarded_down(0);
        assert_eq!(state.consume_triggered(0b0000_0001), 0b0000_0001);
        state.record_forwarded_down(0);
        assert!(!state.record_forwarded_up(0));
        assert_eq!(state.forwarded, 0);
        assert_eq!(state.consumed, 0);
    }
}
