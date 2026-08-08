// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows sandbox using `SendInput` for injection and a secondary
//! `WH_KEYBOARD_LL` hook for monitoring.
//!
//! The sandbox installs its own low-level keyboard hook that runs alongside
//! the daemon's hook.  Injected input events carry a custom `dwExtraInfo`
//! marker so the monitoring callback can distinguish "input" (marked) from
//! "output" (unmarked, emitted by the daemon via `SendInput`).

use std::sync::{
    Arc, Mutex,
};

use windows_sys::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, SendInput,
            VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            CallNextHookEx, KBDLLHOOKSTRUCT, SetWindowsHookExW,
            UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
            WM_SYSKEYDOWN, WM_SYSKEYUP,
        },
    },
};

use super::{CapturedEvent, Sandbox, SandboxError};

/// Type alias for hook handles not re-exported in windows-sys 0.61.
#[allow(clippy::upper_case_acronyms)]
type HHOOK = *mut std::ffi::c_void;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// `dwExtraInfo` marker value for events injected by the sandbox.  A fixed
/// high-bit pattern (0x5Bxx_xxxx) that is unlikely to collide with real
/// `dwExtraInfo` values.  The lower 16 bits are seeded with a compile-time
/// constant; runtime uniqueness is not required since each test run uses its
/// own sandbox instance.
const SEND_MARKER: usize = 0x5BAD_C0DE;

// ---------------------------------------------------------------------------
// Event queue
// ---------------------------------------------------------------------------

/// Thread-safe event queue shared between the hook callback and the test
/// thread.
struct EventQueue {
    events: Mutex<Vec<CapturedEvent>>,
}

impl EventQueue {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    fn push(&self, code: u16, is_down: bool) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(CapturedEvent { code, is_down });
    }

    fn drain(&self) -> Vec<CapturedEvent> {
        std::mem::take(
            &mut self.events.lock().unwrap_or_else(|e| e.into_inner()),
        )
    }
}

// ---------------------------------------------------------------------------
// Monitor hook state
// ---------------------------------------------------------------------------

/// Shared state bridged into the FFI callback via a static.  Because
/// `WH_KEYBOARD_LL` callbacks cannot capture user data, we use a module-level
/// static guarded by a mutex (Rust 2024 prohibits raw references to mutable
/// statics).
static MONITOR_QUEUE: std::sync::Mutex<Option<Arc<EventQueue>>> =
    std::sync::Mutex::new(None);

/// Returns a clone of the current event queue, or `None` if not set.
fn monitor_queue() -> Option<Arc<EventQueue>> {
    MONITOR_QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Sets the shared event queue for the monitor hook.  Pass `None` to clear.
fn set_monitor_queue(queue: Option<Arc<EventQueue>>) {
    *MONITOR_QUEUE.lock().unwrap_or_else(|e| e.into_inner()) = queue;
}

// ---------------------------------------------------------------------------
// Sandbox implementation
// ---------------------------------------------------------------------------

/// Windows sandbox for end-to-end keyboard mapping tests.
pub struct WindowsSandbox {
    /// Handle of the monitoring `WH_KEYBOARD_LL` hook.  Non-null after a
    /// successful `setup()` call.
    hook_handle: HHOOK,

    /// Shared event queue populated by the monitoring hook.
    queue: Arc<EventQueue>,

    /// Flag indicating whether `setup()` has been called successfully.
    is_setup: bool,
}

impl WindowsSandbox {
    /// Inject a synthetic keyboard event using `SendInput`.
    fn inject_key(
        &self,
        code: u16,
        is_down: bool,
    ) -> Result<(), SandboxError> {
        let flags = if is_down { 0 } else { 0x0002 }; // KEYEVENTF_KEYUP

        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: code as VIRTUAL_KEY,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: SEND_MARKER,
                },
            },
        };

        let result = unsafe {
            SendInput(
                1,
                std::ptr::addr_of!(input),
                std::mem::size_of::<INPUT>() as i32,
            )
        };

        if result != 1 {
            Err(SandboxError::InjectionFailed(format!(
                "SendInput returned {result}, expected 1"
            )))
        } else {
            Ok(())
        }
    }
}

impl Sandbox for WindowsSandbox {
    fn new() -> Result<Option<Self>, SandboxError> {
        // `WH_KEYBOARD_LL` and `SendInput` do not require elevation; they
        // operate on the current desktop session.  No prerequisite check is
        // needed.
        Ok(Some(Self {
            hook_handle: std::ptr::null_mut(),
            queue: Arc::new(EventQueue::new()),
            is_setup: false,
        }))
    }

    fn setup(&mut self) -> Result<(), SandboxError> {
        // Install the monitoring `WH_KEYBOARD_LL` hook.  Because the hook
        // procedure is defined in this binary, it must be installed on the
        // thread that will receive callbacks.  `WH_KEYBOARD_LL` callbacks are
        // delivered to the thread that called `SetWindowsHookExW`, so we
        // install on the current (test) thread.

        set_monitor_queue(Some(Arc::clone(&self.queue)));

        let h_instance: HINSTANCE =
            unsafe { GetModuleHandleW(std::ptr::null::<u16>()) };

        let handle: HHOOK = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(monitor_keyboard_proc),
                h_instance,
                0,
            )
        };

        if handle.is_null() {
            set_monitor_queue(None);
            return Err(SandboxError::DeviceCreationFailed(
                "failed to install monitoring keyboard hook".to_string(),
            ));
        }

        self.hook_handle = handle;
        self.is_setup = true;
        Ok(())
    }

    fn inject_key_down(&self, code: u16) -> Result<(), SandboxError> {
        self.inject_key(code, true)
    }

    fn inject_key_up(&self, code: u16) -> Result<(), SandboxError> {
        self.inject_key(code, false)
    }

    fn drain_output_events(&self) -> Vec<CapturedEvent> {
        // Brief pause to allow the hook chain and any `SendInput` calls from
        // the daemon to complete before draining.
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.queue.drain()
    }

    fn input_device_id(&self) -> Option<&str> {
        // Windows hooks are global per desktop; no specific device targeting
        // is needed.
        None
    }

    fn teardown(&mut self) {
        if !self.is_setup {
            return;
        }

        if !self.hook_handle.is_null() {
            unsafe {
                UnhookWindowsHookEx(self.hook_handle);
            }
            self.hook_handle = std::ptr::null_mut();
        }

        // Clear the shared queue reference.
        set_monitor_queue(None);

        self.is_setup = false;
    }
}

impl Drop for WindowsSandbox {
    fn drop(&mut self) {
        self.teardown();
    }
}

// ---------------------------------------------------------------------------
// Monitor hook callback
// ---------------------------------------------------------------------------

/// Low-level keyboard hook procedure that records non-marked events (daemon
/// output) into the shared queue and passes all events through the chain.
///
/// # Safety
/// Called by the Windows message dispatcher.  Must not panic and must follow
/// the standard hook contract: return `CallNextHookEx` to pass the event on,
/// or non-zero to swallow it.  We always pass through.
extern "system" fn monitor_keyboard_proc(
    code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if code < 0 {
        return unsafe {
            CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param)
        };
    }

    let kbd_struct = unsafe { *(l_param as *const KBDLLHOOKSTRUCT) };

    // Only record events that are NOT from our injector.  The absence of the
    // marker means the event was either a real key press or, more relevantly,
    // injected by the daemon's `SendInput` (which sets `dwExtraInfo` to 0).
    if kbd_struct.dwExtraInfo != SEND_MARKER {
        let Some(queue) = monitor_queue() else {
            return unsafe {
                CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param)
            };
        };

        let vk = kbd_struct.vkCode as u16;
        let msg = w_param as u32;
        let is_down = matches!(
            msg,
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP
        );

        // Only capture key-down and key-up; ignore key-repeat which is
        // reported as `WM_KEYDOWN` with the repeat count > 1 in the
        // `wParam` high word.  For simplicity we record all key-down/up
        // events the hook sees — the test layer filters if needed.
        queue.push(vk, is_down);
    }

    // Always pass the event through — we are only monitoring.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, w_param, l_param) }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_new_succeeds() {
        let result = WindowsSandbox::new();
        assert!(result.is_ok(), "new() should succeed on Windows");
        assert!(
            result.as_ref().unwrap().is_some(),
            "new() should return Some"
        );
    }

    #[test]
    fn sandbox_setup_and_teardown() {
        let mut sandbox = WindowsSandbox::new().unwrap().unwrap();
        let setup_result = sandbox.setup();
        assert!(
            setup_result.is_ok(),
            "setup() should succeed: {}",
            setup_result.err().unwrap()
        );

        sandbox.teardown();
        assert!(!sandbox.is_setup);
    }

    #[test]
    fn send_marker_is_nonzero() {
        assert_ne!(SEND_MARKER, 0, "marker must not be zero");
    }

    #[test]
    fn event_queue_push_and_drain() {
        let queue = Arc::new(EventQueue::new());
        queue.push(0xA2, true);
        queue.push(0xA2, false);

        let events = queue.drain();
        assert_eq!(
            events,
            vec![
                CapturedEvent {
                    code: 0xA2,
                    is_down: true,
                },
                CapturedEvent {
                    code: 0xA2,
                    is_down: false,
                },
            ]
        );

        // Queue is empty after drain.
        assert!(queue.drain().is_empty());
    }

    #[test]
    fn captured_event_equality() {
        let a = CapturedEvent {
            code: 0xA2,
            is_down: true,
        };
        let b = CapturedEvent {
            code: 0xA2,
            is_down: true,
        };
        let c = CapturedEvent {
            code: 0xA2,
            is_down: false,
        };

        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn sandbox_error_display() {
        let e = SandboxError::DeviceCreationFailed("test".to_string());
        assert!(format!("{e}").contains("device creation failed"));

        let e = SandboxError::InjectionFailed("test".to_string());
        assert!(format!("{e}").contains("injection failed"));

        let e = SandboxError::NotSupported("test".to_string());
        assert!(format!("{e}").contains("not supported"));
    }
}
