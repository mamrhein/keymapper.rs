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
//! The hook thread installs a \`WH_KEYBOARD_LL\` hook and runs the message
//! loop.  On each key event the hook proc performs the mapping lookup
//! itself — matching the event against the raw input buffer for device
//! identification — and either emits the mapped output directly via
//! \`SendInput\` and swallows the key, or passes it through.
//!
//! The in-callback emission is validated by the capture-mode e2e tests: the
//! daemon's tagged re-emissions are issued from within the hook callback and
//! captured by the monitor's own hook in a separate process, so the previous
//! design (worker thread, one-shot reply channel, deferred emission) is no
//! longer load-bearing.
//!
//! Thread layout:
//!
//! 1. **Hook thread** — \`WH_KEYBOARD_LL\` hook + message loop
//!    (\`MsgWaitForMultipleObjects\` + \`PeekMessageW\`).  Decides and emits
//!    in-callback, and drains the emission queue (fed only by standalone
//!    consumer events) after each wait.
//! 2. **Raw input thread** — Message-only window + \`GetMessageW\` loop for
//!    \`WM_INPUT\`.  Maintains the device-identification buffer and
//!    processes standalone Consumer Control events, which never reach the
//!    hook.

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
    device_match::{device_cache, match_usage},
    key::{Key, hid_to_vk},
    raw_input::start_raw_input_loop,
    raw_worker::spawn_raw_worker,
    INJECTED_TAG,
};
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

/// The compiled mapping cache, shared with the hook proc.
///
/// The hook proc is a `extern "system"` fn and cannot capture locals, so
/// the lookup is parked in a process-wide static that is set once from
/// [`start_mapping`] before the hook is installed.  The hook proc treats a
/// missing lookup as "pass through" so a key event can never stall the
/// input chain while the engine is not up.
static LOOKUP: std::sync::OnceLock<Arc<RwLock<dyn Lookup>>> =
    std::sync::OnceLock::new();

fn set_lookup(lookup: Arc<RwLock<dyn Lookup>>) {
    let _ = LOOKUP.set(lookup);
}

fn lookup() -> Option<&'static Arc<RwLock<dyn Lookup>>> {
    LOOKUP.get()
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

/// Capture-mode debug log, appended to from the hook and raw input threads.
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
/// keyboard).
///
/// Only compiled in with the `e2e` feature: without it, capture mode can
/// never be enabled, so the query is dead code in production builds.
#[cfg(feature = "e2e")]
pub(super) fn capture_enabled() -> bool {
    CAPTURE_MODE.load(Ordering::Relaxed)
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

/// Process-global forwarded-modifier state, mutated only by the hook proc
/// (the sole decider of mapped keyboard events in capture mode).
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

    // A virtual-key code without an HID identity can never match a compiled
    // rule, so pass it through before doing any other work.
    let Some(usage) = usage else {
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

    // Perform the mapping lookup: app-scoped rules for the active
    // application take precedence over global rules, and both honour the
    // device filter resolved above.
    let Some(lookup) = lookup() else {
        // The engine is not up (or is shutting down); never block the
        // input chain on that.
        return unsafe {
            CallNextHookEx(Some(hook_handle()), code, w_param, l_param)
        };
    };
    let guard = lookup.read();
    let outputs = guard
        .for_active_app(usage, pressed_modifiers, device_path.as_deref())
        .or_else(|| {
            guard.global(usage, pressed_modifiers, device_path.as_deref())
        })
        .map(|v| v.to_vec());
    drop(guard);

    // Capture mode (e2e only): the hook proc performs all emission in
    // callback, so the monitor's hook observes it.  Mapped outputs are
    // emitted as complete taps; an unmapped key is forwarded as-is.
    #[cfg(feature = "e2e")]
    if capture_enabled() {
        let own_bit = HidUsage::hid_usage_to_modifier_bit(usage);
        match &outputs {
            Some(outputs) => {
                if !is_key_up {
                    // The trigger's modifiers were forwarded when pressed.
                    // Release them on the virtual keyboard now so the
                    // output is emitted as a clean tap; mark them consumed
                    // so their physical release is swallowed rather than
                    // forwarded a second time.
                    capture_release_triggered_modifiers(pressed_modifiers);
                    for native_key in outputs {
                        emit_key_event(native_key);
                    }
                }
                // Swallow the physical key in both directions: the daemon
                // fully owns it in capture mode.
                return LRESULT(1);
            }
            None => {
                if is_key_up
                    && let Some(bit) = own_bit
                    && capture_record_forwarded_up(bit)
                {
                    // The modifier was already released on the virtual
                    // keyboard when its trigger fired; swallow the
                    // physical release.
                } else {
                    if !is_key_up && let Some(bit) = own_bit {
                        capture_record_forwarded_down(bit);
                    }
                    emit_forwarded_key(vk_code.0, is_key_up);
                }
                // The forwarded re-emission carries the tag, so swallow the
                // physical key to avoid double delivery.
                return LRESULT(1);
            }
        }
    }

    // Normal mode: emit the mapped output directly in the callback and
    // swallow the original key.  A `SendInput` issued from within a
    // `WH_KEYBOARD_LL` callback reaches other hooks and the target window
    // (the capture-mode e2e tests capture the tagged re-emission from a
    // separate process's hook), so the previous deferred-emission design is
    // not needed.
    if !is_key_up {
        if let Some(outputs) = &outputs {
            for native_key in outputs {
                emit_key_event(native_key);
            }
            return LRESULT(1);
        }
    } else if outputs.is_some() {
        // Swallow the key-up to stay consistent with its key-down, which
        // was swallowed when it fired the rule.
        return LRESULT(1);
    }

    unsafe { CallNextHookEx(Some(hook_handle()), code, w_param, l_param) }
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
/// Initialises the raw input loop, spawns the raw input thread, installs
/// the `WH_KEYBOARD_LL` hook, and runs the message loop.  Blocks the
/// calling thread until the message loop exits (i.e. on `WM_QUIT`).
///
/// This is the entry point called by `keymapperd.rs`.
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

    // Capture mode (e2e only, gated on `KEYMAPPER_CAPTURE`): the daemon
    // swallows every key and re-emits it through the virtual keyboard, tagged
    // with [`INJECTED_TAG`], so the monitor's `WH_KEYBOARD_LL` hook can capture
    // the output without depending on a focused window.  Emission happens in
    // the hook proc's callback, which the monitor observes.  Compiled in only
    // with the `e2e` feature, so production builds can never be switched into
    // capture mode via the environment.
    #[cfg(feature = "e2e")]
    if std::env::var("KEYMAPPER_CAPTURE").is_ok_and(|v| !v.is_empty()) {
        set_capture_mode(true);
        eprintln!("Windows: capture mode enabled (KEYMAPPER_CAPTURE).");
    }

    // Record this thread's id so the raw input thread can post the drain
    // wake message to the message loop (done before the raw worker starts,
    // so no emission can be queued before the id is recorded).
    set_main_thread_id(unsafe { GetCurrentThreadId() });

    // Park the lookup where the hook proc can find it, then spawn the raw
    // input thread (which consumes the raw input channel).
    set_lookup(Arc::clone(&lookup));
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

    // The raw input loop, raw input thread, and keyboard hook are all live,
    // so the daemon can now process events.
    if let Some(signal) = ready_signal {
        signal();
    }

    // Run the message loop until WM_QUIT.  The low-level hook callback runs
    // re-entrantly inside the blocked `MsgWaitForMultipleObjects` call and
    // emits mapped keyboard output itself, so the only queueing emission is
    // from standalone consumer events: the wake for the drain is the `WM_APP`
    // message the raw input thread posts through `queue_emission` when it
    // queues a consumer output.  The loop blocks in
    // `MsgWaitForMultipleObjects` (which returns for any posted message),
    // then drains the queue with the non-blocking `PeekMessageW` — a
    // blocking `GetMessageW` here would consume the wake message and then
    // block on the next one, starving the drain.  Messages are removed but
    // neither translated nor dispatched: the hook callback already handled
    // everything else.
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
