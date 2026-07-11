// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventSource, CGEventSourceStateID,
    CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CGKeyCode,
};
use parking_lot::RwLock;

use crate::{mapping_cache::NativeAction, state::Lookup};

/// Shared mutable state bridged into the C callback via `user_info`.
struct TapContext {
    /// Trait-object lookup: decouples this module from RuntimeState's shape.
    lookup: Arc<RwLock<dyn Lookup>>,
    /// Pre-created event source reused for every synthetic keyboard event.
    /// Avoids a per-keystroke allocation inside the hot callback path.
    source: CFRetained<CGEventSource>,
}

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Holds the tap, run-loop-source, and callback context so they stay alive
/// for the lifetime of the event-loop, and are cleanly reclaimed on drop.
struct EventTapHandle {
    tap: CFRetained<CFMachPort>,
    #[allow(dead_code)]
    run_loop_source: CFRetained<CFRunLoopSource>,
    /// Raw pointer to the heap-allocated `TapContext` passed as `user_info`.
    /// Reclaimed in `Drop` to avoid a memory leak.
    context_ptr: *mut TapContext,
}

impl Drop for EventTapHandle {
    fn drop(&mut self) {
        // Disable the tap so no further callbacks fire.
        CGEvent::tap_enable(&self.tap, false);

        // Reclaim and free the leaked `Box<TapContext>`. This also drops
        // the `CFRetained<CGEventSource>`, releasing the CoreFoundation
        // object.
        unsafe {
            drop(Box::from_raw(self.context_ptr));
        }
    }
}

/// Async-signal-safe handler that flips the shutdown flag.
extern "C" fn signal_handler(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mask: u64 =
        (1u64 << CGEventType::KeyDown.0) | (1u64 << CGEventType::KeyUp.0);

    let source =
        CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .ok_or("Failed to create CGEventSource")?;

    // Allocate the context on the heap and leak the `Box` to get a stable
    // pointer for the FFI callback.  The `EventTapHandle` owns this pointer
    // and reclaims it in `Drop`.
    let context_ptr =
        Box::into_raw(Box::new(TapContext { lookup, source })) as *mut _;

    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(macos_keyboard_callback_ffi),
            context_ptr as *mut c_void,
        )
    };

    let Some(tap) = tap else {
        // Tap creation failed; reclaim the context to avoid the leak.
        unsafe {
            drop(Box::from_raw(context_ptr));
        }
        return Err("Failed to create macOS CGEventTap. Verify \
                    Accessibility privileges!"
            .into());
    };

    let Some(run_loop_source) =
        CFMachPort::new_run_loop_source(None, Some(&tap), 0)
    else {
        unsafe {
            drop(Box::from_raw(context_ptr));
        }
        return Err("Failed to create CFRunLoopSource from Mach Port.".into());
    };

    let run_loop = CFRunLoop::current().ok_or("No current run loop")?;
    run_loop
        .add_source(Some(&run_loop_source), unsafe { kCFRunLoopCommonModes });

    CGEvent::tap_enable(&tap, true);
    println!("Modern compile-safe macOS Event Tap actively running...");

    // Register signal handlers for graceful shutdown.
    let handler_ptr = signal_handler as *const () as usize;
    unsafe {
        libc::signal(libc::SIGINT, handler_ptr);
        libc::signal(libc::SIGTERM, handler_ptr);
    }

    // Own the tap, run-loop-source, and context pointer. Dropped together
    // when the run-loop exits.
    let handle = EventTapHandle {
        tap,
        run_loop_source,
        context_ptr,
    };

    // Poll the run-loop with a short timeout so we can check the shutdown
    // flag each iteration. This avoids an infinite `CFRunLoop::run()` block
    // and lets us exit cleanly on SIGINT / SIGTERM.
    while !SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopCommonModes },
            0.5, // 500 ms timeout
            true,
        );
    }

    println!("Shutdown signal received. Cleaning up...");

    // `handle` is dropped here, which:
    // 1. Disables the tap
    // 2. Reclaims and frees the `TapContext` (and its `CGEventSource`)
    drop(handle);

    Ok(())
}

/// FFI callback invoked by the event tap for every matching keyboard event.
///
/// # Safety
/// Called from CoreGraphics on the run-loop thread.  `proxy` and `user_info`
/// are managed by the system / our `TapContext`.
unsafe extern "C-unwind" fn macos_keyboard_callback_ffi(
    _proxy: objc2_core_graphics::CGEventTapProxy,
    _type: CGEventType,
    event: core::ptr::NonNull<objc2_core_graphics::CGEvent>,
    user_info: *mut std::ffi::c_void,
) -> *mut objc2_core_graphics::CGEvent {
    if user_info.is_null() {
        return event.as_ptr();
    }

    let context = unsafe { &*(user_info as *const TapContext) };

    // CGKeyCode is u16 — matches NativeKey directly, no cast needed.
    let native_key = unsafe {
        CGEvent::integer_value_field(
            Some(event.as_ref()),
            CGEventField::KeyboardEventKeycode,
        )
    } as CGKeyCode;

    let is_down = _type == CGEventType::KeyDown;

    // Resolve the remapping through the trait interface.
    let guard = context.lookup.read();
    let current_app = guard.active_app().to_lowercase();
    let active_action = guard
        .for_app(&current_app, native_key)
        .or_else(|| guard.global(native_key));

    if let Some(action) = active_action {
        match action {
            NativeAction::RemapTo(target_code) => {
                // Modify the existing event's keycode in place.
                unsafe {
                    CGEvent::set_integer_value_field(
                        Some(event.as_ref()),
                        CGEventField::KeyboardEventKeycode,
                        *target_code as i64,
                    );
                }
                return event.as_ptr();
            }
            NativeAction::Shortcut(target_codes) => {
                let source = &context.source;
                if is_down {
                    for code in target_codes.iter() {
                        if let Some(e) = CGEvent::new_keyboard_event(
                            Some(source),
                            *code as CGKeyCode,
                            true,
                        ) {
                            CGEvent::post(
                                CGEventTapLocation::HIDEventTap,
                                Some(&e),
                            );
                        }
                    }
                } else {
                    for code in target_codes.iter().rev() {
                        if let Some(e) = CGEvent::new_keyboard_event(
                            Some(source),
                            *code as CGKeyCode,
                            false,
                        ) {
                            CGEvent::post(
                                CGEventTapLocation::HIDEventTap,
                                Some(&e),
                            );
                        }
                    }
                }
                // Suppress the original event for shortcuts.
                return std::ptr::null_mut();
            }
        }
    }

    event.as_ptr()
}
