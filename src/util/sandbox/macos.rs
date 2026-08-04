// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS sandbox using CGEventTap for monitoring and CGEvent for injection.
//!
//! The monitor tap runs on a dedicated thread with its own CFRunLoop,
//! capturing all keyboard events at the HID level.  Injected events are
//! posted at HIDEventTap so the daemon's event tap sees them.

use std::{
    ffi::c_void,
    marker::PhantomData,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, kCFRunLoopCommonModes,
    kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventSource, CGEventSourceStateID,
    CGEventTapLocation, CGEventTapPlacement, CGEventType, CGKeyCode,
};

use super::{CapturedEvent, Sandbox, SandboxError};

// ---------------------------------------------------------------------------
// Unsafe Send wrappers for CoreFoundation types
// ---------------------------------------------------------------------------

/// Wraps a `CFRetained<T>` to assert `Send` across thread boundaries.
///
/// # Safety
/// CoreFoundation types use internal locking for reference counting and are
/// documented as thread-safe for retention/release.  The wrapped object must
/// not be mutated from multiple threads simultaneously — in our case the tap
/// and run-loop-source are only used on the monitor thread after creation.
struct SendCf<T> {
    inner: CFRetained<T>,
    _phantom: PhantomData<*mut ()>,
}

impl<T> SendCf<T> {
    fn new(inner: CFRetained<T>) -> Self {
        Self {
            inner,
            _phantom: PhantomData,
        }
    }

    fn inner(&self) -> &CFRetained<T> {
        &self.inner
    }

    fn into_inner(self) -> CFRetained<T> {
        self.inner
    }
}

unsafe impl<T> Send for SendCf<T> {}

// ---------------------------------------------------------------------------
// Shared state between the monitor tap thread and the main sandbox object
// ---------------------------------------------------------------------------

/// Event queue shared between the tap callback and the test thread.
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

/// Shared state bridged into the FFI callback via `user_info`.
struct MonitorContext {
    queue: Arc<EventQueue>,
}

/// Owning handle for the monitor tap and its background thread.
struct MonitorHandle {
    /// Raw pointer to the heap-allocated `MonitorContext` passed to the tap
    /// callback as `user_info`.  Freed during teardown.
    context_ptr: *mut MonitorContext,

    /// Shutdown flag used to stop the monitor thread.
    shutdown_flag: Arc<AtomicBool>,

    /// Handle to the background thread running the CFRunLoop.
    thread_handle: Option<thread::JoinHandle<()>>,
}

// ---------------------------------------------------------------------------
// Sandbox implementation
// ---------------------------------------------------------------------------

/// macOS sandbox for end-to-end keyboard mapping tests.
pub struct MacoSandbox {
    /// Shared event queue between the monitor tap and the test thread.
    queue: Arc<EventQueue>,

    /// Handle to the monitor tap and its background thread.
    handle: Option<MonitorHandle>,

    /// Flag indicating whether `setup()` has been called successfully.
    is_setup: bool,
}

impl MacoSandbox {
    /// Check if the current process has the permissions needed to create
    /// an event tap.
    fn check_permissions() -> Result<(), SandboxError> {
        let mask =
            (1u64 << CGEventType::KeyDown.0) | (1u64 << CGEventType::KeyUp.0);

        let probe = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::HIDEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                objc2_core_graphics::CGEventTapOptions::Default,
                mask,
                None,
                std::ptr::null_mut(),
            )
        };

        if probe.is_some() {
            Ok(())
        } else {
            Err(SandboxError::PermissionDenied(
                "Accessibility permission required. Grant it in System \
                 Settings > Privacy & Security > Accessibility."
                    .to_string(),
            ))
        }
    }

    /// Inject a keyboard event using a freshly created event source.
    fn inject_key(
        &self,
        code: u16,
        is_down: bool,
    ) -> Result<(), SandboxError> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok_or_else(|| {
                SandboxError::InjectionFailed(
                    "failed to create CGEventSource for injection".to_string(),
                )
            })?;

        let Some(event) =
            CGEvent::new_keyboard_event(Some(&source), code, is_down)
        else {
            return Err(SandboxError::InjectionFailed(format!(
                "failed to create keyboard event for code {code}"
            )));
        };

        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        thread::sleep(Duration::from_millis(5));
        Ok(())
    }
}

impl Sandbox for MacoSandbox {
    fn new() -> Result<Option<Self>, SandboxError> {
        Self::check_permissions()?;

        Ok(Some(Self {
            queue: Arc::new(EventQueue::new()),
            handle: None,
            is_setup: false,
        }))
    }

    fn setup(&mut self) -> Result<(), SandboxError> {
        let mask =
            (1u64 << CGEventType::KeyDown.0) | (1u64 << CGEventType::KeyUp.0);

        let context = Box::into_raw(Box::new(MonitorContext {
            queue: Arc::clone(&self.queue),
        }));

        let shutdown = Arc::new(AtomicBool::new(false));

        // Create the tap on the main thread, then move it into the monitor
        // thread via SendCf wrappers.
        let tap = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::HIDEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                objc2_core_graphics::CGEventTapOptions::Default,
                mask,
                Some(monitor_callback_ffi),
                context as *mut c_void,
            )
        }
        .ok_or_else(|| {
            unsafe {
                drop(Box::from_raw(context));
            }
            SandboxError::DeviceCreationFailed(
                "failed to create monitor event tap. Verify Accessibility \
                 privileges are granted to the test process."
                    .to_string(),
            )
        })?;

        // Move ownership of the tap into the spawned thread.  The CFRunLoop
        // and its source must be created on the same thread that runs them.
        let shutdown_clone = Arc::clone(&shutdown);
        let tap_owned = SendCf::new(tap);

        let thread_handle = thread::Builder::new()
            .name("sandbox-monitor".to_string())
            .spawn(move || {
                // Create run-loop-source on this thread.
                let tap = tap_owned.into_inner();
                let run_loop_source =
                    CFMachPort::new_run_loop_source(None, Some(&tap), 0)
                        .expect("failed to create CFRunLoopSource");

                let run_loop =
                    CFRunLoop::current().expect("no current run loop");

                run_loop.add_source(Some(&run_loop_source), unsafe {
                    kCFRunLoopCommonModes
                });

                CGEvent::tap_enable(&tap, true);

                while !shutdown_clone.load(Ordering::Acquire) {
                    CFRunLoop::run_in_mode(
                        unsafe { kCFRunLoopDefaultMode },
                        0.1,
                        true,
                    );
                }

                CGEvent::tap_enable(&tap, false);
                // `tap` and `run_loop_source` are dropped here, cleaning up
                // the CoreFoundation objects.
            })
            .map_err(|e| {
                // `tap_owned` was moved into the closure and is dropped here
                // (releasing the CFMachPort). We only need to free the
                // context.
                unsafe {
                    drop(Box::from_raw(context));
                }
                SandboxError::DeviceCreationFailed(format!(
                    "failed to spawn monitor thread: {e}"
                ))
            })?;

        self.handle = Some(MonitorHandle {
            context_ptr: context,
            shutdown_flag: shutdown,
            thread_handle: Some(thread_handle),
        });

        // Give the tap a moment to start processing events.
        thread::sleep(Duration::from_millis(200));

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
        // Brief pause to allow pending events to be captured.
        thread::sleep(Duration::from_millis(50));
        self.queue.drain()
    }

    fn input_device_id(&self) -> Option<&str> {
        None
    }

    fn teardown(&mut self) {
        if !self.is_setup {
            return;
        }

        let Some(mut handle) = self.handle.take() else {
            return;
        };

        // Signal the monitor thread to stop.
        handle.shutdown_flag.store(true, Ordering::Release);

        // Wait for the thread to finish.  The tap and run-loop-source are
        // dropped inside the thread when it exits.
        if let Some(jh) = handle.thread_handle.take() {
            let _ = jh.join();
        }

        // Free the callback context.
        unsafe {
            drop(Box::from_raw(handle.context_ptr));
        }

        self.is_setup = false;
    }
}

impl Drop for MacoSandbox {
    fn drop(&mut self) {
        self.teardown();
    }
}

// ---------------------------------------------------------------------------
// Monitor tap FFI callback
// ---------------------------------------------------------------------------

/// Record every keyboard event into the shared queue and pass it through.
unsafe extern "C-unwind" fn monitor_callback_ffi(
    _proxy: objc2_core_graphics::CGEventTapProxy,
    event_type: CGEventType,
    event: core::ptr::NonNull<objc2_core_graphics::CGEvent>,
    user_info: *mut c_void,
) -> *mut objc2_core_graphics::CGEvent {
    if user_info.is_null() {
        return event.as_ptr();
    }

    let context = unsafe { &*(user_info as *const MonitorContext) };

    let is_down = event_type == CGEventType::KeyDown;
    let code: CGKeyCode = unsafe {
        CGEvent::integer_value_field(
            Some(event.as_ref()),
            CGEventField::KeyboardEventKeycode,
        )
    } as CGKeyCode;

    context.queue.push(code, is_down);

    // Pass the event through — we are only monitoring, not intercepting.
    event.as_ptr()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_new_returns_some_or_permission_error() {
        let result = MacoSandbox::new();
        assert!(
            result.is_ok(),
            "new() should return Ok (containing Some or a PermissionDenied)"
        );
    }

    #[test]
    fn captured_event_equality() {
        let a = CapturedEvent {
            code: 0,
            is_down: true,
        };
        let b = CapturedEvent {
            code: 0,
            is_down: true,
        };
        let c = CapturedEvent {
            code: 0,
            is_down: false,
        };
        let d = CapturedEvent {
            code: 1,
            is_down: true,
        };

        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    #[test]
    fn sandbox_error_display() {
        let e = SandboxError::PermissionDenied("test".to_string());
        assert!(format!("{e}").contains("permission denied"));

        let e = SandboxError::DeviceCreationFailed("test".to_string());
        assert!(format!("{e}").contains("device creation failed"));

        let e = SandboxError::InjectionFailed("test".to_string());
        assert!(format!("{e}").contains("injection failed"));

        let e = SandboxError::NotSupported("test".to_string());
        assert!(format!("{e}").contains("not supported"));
    }

    #[test]
    fn sandbox_error_is_std_error() {
        let e: Box<dyn std::error::Error> =
            Box::new(SandboxError::NotSupported("test".into()));
        assert!(!e.to_string().is_empty());
    }
}
