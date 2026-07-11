// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::sync::Arc;

use objc2_core_foundation::{
    CFMachPort, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventSource, CGEventSourceStateID,
    CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CGKeyCode,
};
use parking_lot::RwLock;

use crate::{RuntimeState, mapping_cache::NativeAction};

/// Shared mutable state bridged into the C callback via `user_info`.
struct TapContext {
    state: Arc<RwLock<RuntimeState>>,
}

/// Holds the tap and run-loop-source so they stay alive for the lifetime
/// of the event-loop.
struct EventTapHandle {
    #[allow(dead_code)]
    tap: objc2_core_foundation::CFRetained<CFMachPort>,
    #[allow(dead_code)]
    run_loop_source: objc2_core_foundation::CFRetained<CFRunLoopSource>,
}

pub fn start_mapping(
    state: Arc<RwLock<RuntimeState>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mask: u64 =
        (1u64 << CGEventType::KeyDown.0) | (1u64 << CGEventType::KeyUp.0);

    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(macos_keyboard_callback_ffi),
            Box::into_raw(Box::new(TapContext { state })) as _,
        )
    };

    let Some(tap) = tap else {
        return Err("Failed to create macOS CGEventTap. Verify \
                    Accessibility privileges!"
            .into());
    };

    let Some(run_loop_source) =
        CFMachPort::new_run_loop_source(None, Some(&tap), 0)
    else {
        return Err("Failed to create CFRunLoopSource from Mach Port.".into());
    };

    if let Some(rl) = CFRunLoop::current() {
        rl.add_source(Some(&run_loop_source), unsafe {
            kCFRunLoopCommonModes
        });
    }

    CGEvent::tap_enable(&tap, true);
    println!("Modern compile-safe macOS Event Tap actively running...");

    // Keep the tap + run-loop-source alive while we block on the run-loop.
    let _handle = EventTapHandle {
        tap,
        run_loop_source,
    };

    CFRunLoop::run();

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
    let state = &context.state;

    let native_key = unsafe {
        CGEvent::integer_value_field(
            Some(event.as_ref()),
            CGEventField::KeyboardEventKeycode,
        )
    } as u32;

    let is_down = _type == CGEventType::KeyDown;

    let state_guard = state.read();
    let current_app = state_guard.active_app.to_lowercase();

    let mut active_action = state_guard
        .lookup_cache
        .process_map
        .get(&current_app)
        .and_then(|m| m.get(&native_key));

    if active_action.is_none() {
        active_action = state_guard.lookup_cache.global_map.get(&native_key);
    }

    if let Some(action) = active_action {
        let source =
            CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
                .unwrap();

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
                if is_down {
                    for code in target_codes {
                        if let Some(e) = CGEvent::new_keyboard_event(
                            Some(&source),
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
                            Some(&source),
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
