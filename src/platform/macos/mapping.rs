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
    thread,
    time::Duration,
};

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes,
    kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventSource, CGEventSourceStateID,
    CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CGKeyCode,
};
use parking_lot::RwLock;
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag::register,
};

use super::key::Key;
use crate::{
    common::modifier::ModifierRole,
    daemon::{mapping_cache::NativeKey, state::Lookup},
};

// ---------------------------------------------------------------------------
// Modifier handling — track specific key state for exact matching
// ---------------------------------------------------------------------------

/// Map a CGKeyCode to its modifier bit position.  Returns `None` for
/// non-modifier keys.
/// Map a raw CGKeyCode to its modifier bit position via the shared
/// `ModifierRole` type.
fn keycode_to_modifier_bit(code: CGKeyCode) -> Option<u8> {
    let role = match code {
        59 => ModifierRole::LeftControl, // kVK_Control (left)
        62 => ModifierRole::RightControl, // kVK_RightControl
        56 => ModifierRole::LeftShift,   // kVK_Shift (left)
        60 => ModifierRole::RightShift,  // kVK_RightShift
        58 => ModifierRole::LeftAlt,     // kVK_Option (left)
        61 => ModifierRole::RightAlt,    // kVK_RightOption
        55 => ModifierRole::LeftCommand, // kVK_Command (left)
        54 => ModifierRole::RightCommand, // kVK_RightCommand
        _ => return None,
    };
    Some(role.bit())
}

/// Map a modifier bit position back to the native CGKeyCode for emission.
fn modifier_bit_to_code(bit: u8) -> Option<CGKeyCode> {
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
    Some(key.as_native())
}

/// Emit a single `NativeKey` as a chord: hold modifiers, press base,
/// release base, release modifiers in reverse order.
fn emit_key_event(source: &CFRetained<CGEventSource>, native_key: &NativeKey) {
    let mut pressed_modifiers: Vec<CGKeyCode> = Vec::new();

    // Press modifiers.
    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1
            && let Some(code) = modifier_bit_to_code(bit)
        {
            if let Some(e) =
                CGEvent::new_keyboard_event(Some(source), code, true)
            {
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
            }
            pressed_modifiers.push(code);
            thread::sleep(Duration::from_millis(1));
        }
    }

    // Press base key.
    if let Some(e) = CGEvent::new_keyboard_event(
        Some(source),
        native_key.base as CGKeyCode,
        true,
    ) {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
    }
    thread::sleep(Duration::from_millis(1));

    // Release base key.
    if let Some(e) = CGEvent::new_keyboard_event(
        Some(source),
        native_key.base as CGKeyCode,
        false,
    ) {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
    }
    thread::sleep(Duration::from_millis(1));

    // Release modifiers.
    for code in pressed_modifiers.into_iter() {
        if let Some(e) = CGEvent::new_keyboard_event(Some(source), code, false)
        {
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

// ---------------------------------------------------------------------------
// Event tap implementation
// ---------------------------------------------------------------------------

/// Shared mutable state bridged into the C callback via `user_info`.
struct TapContext {
    /// Trait-object lookup: decouples this module from RuntimeState's shape.
    lookup: Arc<RwLock<dyn Lookup>>,
    /// Pre-created event source reused for every synthetic keyboard event.
    /// Avoids a per-keystroke allocation inside the hot callback path.
    source: CFRetained<CGEventSource>,
    /// Bitmask tracking which specific modifier keys are physically pressed.
    modifier_state: u8,
}

/// Holds the tap, run-loop-source, and callback context so they stay alive
/// for the lifetime of the event-loop, and are cleanly reclaimed on drop.
struct EventTapHandle {
    tap: CFRetained<CFMachPort>,
    #[allow(dead_code)]
    run_loop_source: CFRetained<CFRunLoopSource>,
    /// Raw pointer to the heap-allocated `TapContext` passed as `user_info`.
    context_ptr: *mut TapContext,
}

impl Drop for EventTapHandle {
    fn drop(&mut self) {
        CGEvent::tap_enable(&self.tap, false);
        unsafe {
            drop(Box::from_raw(self.context_ptr));
        }
    }
}

pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mask: u64 =
        (1u64 << CGEventType::KeyDown.0) | (1u64 << CGEventType::KeyUp.0);

    let source =
        CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .ok_or("Failed to create CGEventSource")?;

    let context_ptr = Box::into_raw(Box::new(TapContext {
        lookup,
        source,
        modifier_state: 0,
    })) as *mut _;

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
    println!("macOS Event Tap running.");

    let shutdown = Arc::new(AtomicBool::new(false));
    register(SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    register(SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");

    let handle = EventTapHandle {
        tap,
        run_loop_source,
        context_ptr,
    };

    // Poll the run loop in default mode until shutdown is signaled.
    // `kCFRunLoopCommonModes` is a pseudo-mode that cannot be passed to
    // CFRunLoopRunInMode; `kCFRunLoopDefaultMode` is a member of the common
    // modes set and receives the tap events.
    while !shutdown.load(Ordering::Acquire) {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.5, true);
    }

    println!("Shutdown signal received. Cleaning up...");
    drop(handle);

    Ok(())
}

/// FFI callback invoked by the event tap for every matching keyboard event.
unsafe extern "C-unwind" fn macos_keyboard_callback_ffi(
    _proxy: objc2_core_graphics::CGEventTapProxy,
    _type: CGEventType,
    event: core::ptr::NonNull<objc2_core_graphics::CGEvent>,
    user_info: *mut std::ffi::c_void,
) -> *mut objc2_core_graphics::CGEvent {
    if user_info.is_null() {
        return event.as_ptr();
    }

    let context = unsafe { &mut *(user_info as *mut TapContext) };

    let native_key: CGKeyCode = unsafe {
        CGEvent::integer_value_field(
            Some(event.as_ref()),
            CGEventField::KeyboardEventKeycode,
        )
    } as CGKeyCode;

    let is_down = _type == CGEventType::KeyDown;

    // Capture the modifier state for rule matching before updating it,
    // so that bare-modifier triggers (e.g. "Control: A") match correctly
    // against the concurrent modifier set.
    let lookup_modifiers = context.modifier_state;

    // Track specific modifier key state for exact matching.
    if let Some(bit) = keycode_to_modifier_bit(native_key) {
        if is_down {
            context.modifier_state |= 1 << bit;
        } else {
            context.modifier_state &= !(1 << bit);
        }
    }

    let guard = context.lookup.read();
    let active_outputs = guard
        .for_app(guard.active_app(), native_key, lookup_modifiers, None)
        .or_else(|| guard.global(native_key, lookup_modifiers, None))
        .map(|v| v.to_vec());
    drop(guard);

    if let Some(outputs) = active_outputs {
        // Emit mapped outputs and swallow the original event.  This applies
        // to modifier keys as well: if a bare modifier is mapped, its outputs
        // are emitted and `null` is returned to suppress the original event.
        if is_down {
            for native_key in &outputs {
                emit_key_event(&context.source, native_key);
            }
        }
        return std::ptr::null_mut();
    }

    event.as_ptr()
}
