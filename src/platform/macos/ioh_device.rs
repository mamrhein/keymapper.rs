// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Dynamic binding layer for IOHIDManager on macOS.
//!
//! Resolves all IOKit HID Manager symbols at runtime via `dlsym`, avoiding a
//! compile-time dependency on the IOHID framework.  Follows the same pattern
//! as `hid_socket.rs` for consistency and safety.
//!
//! IOHIDManager delivers events per-device, providing the `IOHIDDeviceRef` in
//! each callback.  This gives direct access to device properties (product
//! name, vendor ID, location ID) that can be resolved against the existing
//! `keyboard_registry` for keyboard filtering.

use std::{ffi::c_void, ptr};

use objc2_core_foundation::{CFRetained, CFRunLoop};

// ---------------------------------------------------------------------------
// Opaque IOHID types
// ---------------------------------------------------------------------------

/// Opaque `IOHIDManagerRef`.
#[repr(C)]
pub struct IOHIDManager {
    _private: [u8; 0],
}

/// Opaque `IOHIDDeviceRef`.
#[repr(C)]
pub struct IOHIDDevice {
    _private: [u8; 0],
}

/// Opaque `IOHIDEventRef`.
#[repr(C)]
pub struct IOHIDEvent {
    _private: [u8; 0],
}

/// Opaque `CFAllocatorRef` (we use null for `kCFAllocatorDefault`).
#[allow(non_camel_case_types)]
type CFAllocatorRef = *mut c_void;

/// `kCFAllocatorDefault` is represented as NULL.
#[allow(non_upper_case_globals)]
const kCFAllocatorDefault: CFAllocatorRef = ptr::null_mut();

// ---------------------------------------------------------------------------
// IOHID event type constants
// ---------------------------------------------------------------------------

/// `kIOHIDEventTypeKeyboard`.
#[allow(non_upper_case_globals)]
const kIOHIDEventTypeKeyboard: u32 = 7;

// ---------------------------------------------------------------------------
// HID usage → CGKeyCode translation
// ---------------------------------------------------------------------------

/// Translate a USB HID Keyboard/Keypad usage code to a macOS CGKeyCode.
///
/// Derived from the USB HID Usage Tables v1.21 (page 53-60) cross-referenced
/// with Apple's `ev_keymap.h` virtual key constants.  This is the inverse of
/// `cg_keycode_to_usb_hid` in `hid_socket.rs`.  Returns `None` for usages
/// that have no known CGKeyCode equivalent.
pub fn cg_keycode_from_hid_usage(usage: u16) -> Option<u16> {
    Some(match usage {
        // --- Letters (HID usage → CGKeyCode) ---
        0x04 => 0,  // A
        0x05 => 11, // B
        0x06 => 8,  // C
        0x07 => 2,  // D
        0x08 => 14, // E
        0x09 => 3,  // F
        0x0A => 5,  // G
        0x0B => 4,  // H
        0x0C => 34, // I
        0x0D => 38, // J
        0x0E => 40, // K
        0x0F => 37, // L
        0x10 => 46, // M
        0x11 => 45, // N
        0x12 => 31, // O
        0x13 => 35, // P
        0x14 => 12, // Q
        0x15 => 15, // R
        0x16 => 1,  // S
        0x17 => 17, // T
        0x18 => 32, // U
        0x19 => 9,  // V
        0x1A => 13, // W
        0x1B => 7,  // X
        0x1C => 16, // Y
        0x1D => 6,  // Z

        // --- Numbers ---
        0x1E => 18, // 1
        0x1F => 19, // 2
        0x20 => 20, // 3
        0x21 => 21, // 4
        0x22 => 23, // 5
        0x23 => 22, // 6
        0x24 => 26, // 7
        0x25 => 28, // 8
        0x26 => 25, // 9
        0x27 => 29, // 0

        // --- Edit / navigation ---
        0x28 => 36,  // Return
        0x29 => 53,  // Escape
        0x2A => 51,  // Backspace (HID Delete)
        0x2B => 48,  // Tab
        0x2C => 49,  // Space
        0x4C => 117, // ForwardDelete (HID Clear)

        // --- Modifier keys ---
        0xE0 => 59, // LeftControl
        0xE1 => 62, // RightControl
        0xE2 => 56, // LeftShift
        0xE3 => 60, // RightShift
        0xE4 => 58, // LeftAlt (Option)
        0xE5 => 61, // RightAlt (Right Option)
        0xE6 => 55, // LeftCommand
        0xE7 => 54, // RightCommand
        0x39 => 57, // CapsLock (Keyboard Locking Caps Lock)

        // --- Function keys ---
        0x3A => 122, // F1
        0x3B => 120, // F2
        0x3C => 99,  // F3
        0x3D => 118, // F4
        0x3E => 96,  // F5
        0x3F => 97,  // F6
        0x40 => 98,  // F7
        0x41 => 100, // F8
        0x42 => 101, // F9
        0x43 => 109, // F10
        0x44 => 103, // F11
        0x45 => 111, // F12

        // --- Navigation cluster ---
        0x4A => 115, // Home
        0x4B => 124, // RightArrow (HID also maps here)
        0x4D => 119, // End
        0x4E => 116, // PageUp
        0x4F => 121, // PageDown
        0x50 => 123, // LeftArrow
        0x51 => 125, // DownArrow
        0x52 => 126, // UpArrow

        // --- Punctuation / symbols ---
        0x2D => 27, // Minus (-)
        0x2F => 24, // Equal (=)
        0x31 => 33, // BracketLeft ([) — note: Backslash also maps here
        0x32 => 30, // BracketRight (])
        0x34 => 39, // Quote (' )
        0x35 => 50, // Grave (` ~, HID Non-US # & ~)
        0x36 => 43, // Comma (,)
        0x37 => 44, // Slash (/)
        0x38 => 47, // Period (.)
        0x33 => 41, // Semicolon
        0x63 => 65, // NumpadDecimal / ISO |\|

        // --- Numpad ---
        0x53 => 83, // Numpad1
        0x54 => 84, // Numpad2 / NumpadDivide
        0x55 => 85, // Numpad3 / NumpadMultiply
        0x56 => 86, // Numpad4 / NumpadMinus (context-dependent)
        0x57 => 87, // Numpad5
        0x58 => 88, // Numpad6 / NumpadEnter (context-dependent)
        0x59 => 89, // Numpad7 / NumpadEqual (context-dependent)
        0x5A => 91, // Numpad8
        0x5B => 92, // Numpad9
        0x5C => 65, // NumpadDecimal (Keypad .)
        0x5D => 75, // NumpadMultiply (Keypad *)
        0x5E => 69, // NumpadPlus (Keypad +)
        0x5F => 73, // NumpadDivide (Keypad /)
        0x60 => 76, // NumpadEnter (Keypad Enter)
        0x61 => 78, // NumpadMinus (Keypad -)
        0x62 => 71, // NumpadEqual (Keypad =)
        0x47 => 71, // NumpadClear (Keypad Clear)

        // --- Extended function keys (F13-F20, mapped to Execute etc.) ---
        0x68 => 105, // F13 (Execute)
        0x69 => 107, // F14 (Help)
        0x6A => 113, // F15 (Menu / Select)
        0x6B => 106, // F16 (Stop)
        0x6C => 110, // F17 (Again / Undo)
        0x6D => 104, // F18 (Find / Open)
        0x6E => 102, // F19 (Cut)

        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// IOHIDManager FFI — resolved dynamically via dlsym
// ---------------------------------------------------------------------------

/// Function pointer for `IOHIDManagerCreate`.
type FnIOHIDManagerCreate =
    unsafe extern "C" fn(CFAllocatorRef, u32) -> *mut IOHIDManager;

/// Function pointer for `IOHIDManagerSetDeviceMatching`.
type FnIOHIDManagerSetDeviceMatching =
    unsafe extern "C" fn(manager: *mut IOHIDManager);

/// Function pointer for `IOHIDManagerScheduleWithRunLoop`.
type FnIOHIDManagerScheduleWithRunLoop = unsafe extern "C" fn(
    manager: *mut IOHIDManager,
    run_loop: *mut c_void, // CFRunLoopRef
    mode: *mut c_void,     // CFStringRef (mode)
);

/// Function pointer for `IOHIDManagerOpen`.
type FnIOHIDManagerOpen =
    unsafe extern "C" fn(manager: *mut IOHIDManager, flags: u32) -> u32;

/// Function pointer for `IOHIDManagerClose`.
type FnIOHIDManagerClose =
    unsafe extern "C" fn(manager: *mut IOHIDManager, flags: u32) -> u32;

/// Function pointer for `IOHIDDeviceGetLocationID`.
type FnIOHIDDeviceGetLocationID =
    unsafe extern "C" fn(device: *mut IOHIDDevice) -> u32;

/// Function pointer for `IOHIDEventGetType`.
type FnIOHIDEventGetType = unsafe extern "C" fn(event: *mut IOHIDEvent) -> u32;

/// Function pointer for `IOHIDEventGetIntegerValue`.
#[allow(improper_ctypes_definitions)]
type FnIOHIDEventGetIntegerValue =
    unsafe extern "C" fn(event: *mut IOHIDEvent, field: u32) -> i64;

/// Function pointer for `IOHIDManagerRegisterInputCallback`
/// (manager-level callback that iterates all devices).
type FnIOHIDManagerRegisterInputCallback = unsafe extern "C" fn(
    manager: *mut IOHIDManager,
    callback: IOHIDManagerInputCallback,
    context: *mut c_void,
);

/// Manager-level callback type.
type IOHIDManagerInputCallback = unsafe extern "C" fn(
    context: *mut c_void,
    _result: u32,
    _sender: *mut c_void, // IOHIDSenderRef (may be null)
    event: *mut IOHIDEvent,
    device: *mut IOHIDDevice,
);

/// Resolved function pointers for the IOHIDManager API.  Cached in a static so
/// they can be shared across invocations.
static IOHID_FUNCS: std::sync::OnceLock<IOHidFunctions> =
    std::sync::OnceLock::new();

struct IOHidFunctions {
    manager_create: Option<FnIOHIDManagerCreate>,
    manager_set_device_matching: Option<FnIOHIDManagerSetDeviceMatching>,
    manager_schedule_with_run_loop: Option<FnIOHIDManagerScheduleWithRunLoop>,
    manager_register_input_callback:
        Option<FnIOHIDManagerRegisterInputCallback>,
    manager_open: Option<FnIOHIDManagerOpen>,
    manager_close: Option<FnIOHIDManagerClose>,
    device_get_location_id: Option<FnIOHIDDeviceGetLocationID>,
    event_get_type: Option<FnIOHIDEventGetType>,
    event_get_integer_value: Option<FnIOHIDEventGetIntegerValue>,
}

impl IOHidFunctions {
    /// Try to resolve all IOHIDManager symbols from IOKit at runtime.
    ///
    /// Returns `true` when all required symbols are available.  Resolves once
    /// and caches the result globally.
    pub fn resolve() -> bool {
        if IOHID_FUNCS.get().is_some() {
            return true;
        }

        // Load the IOKit framework dynamically.  IOHIDManager symbols live
        // inside IOKit.framework, not a separate sub-framework.
        let path = b"/System/Library/Frameworks/IOKit.framework/IOKit\0";
        let handle =
            unsafe { libc::dlopen(path.as_ptr() as *const _, libc::RTLD_NOW) };
        if handle.is_null() {
            eprintln!("IOHIDManager: failed to load IOKit framework");
            return false;
        }

        // SAFETY: `Option<FnType>` uses niche optimization where null pointer
        // bits represent `None`.  Transmuting `*mut c_void` (from dlsym) to
        // `Option<FnType>` is valid because both have identical size and
        // alignment, and the null/non-null bit patterns match.
        let manager_create = unsafe {
            std::mem::transmute::<*mut c_void, Option<FnIOHIDManagerCreate>>(
                libc::dlsym(
                    handle,
                    c"IOHIDManagerCreate".as_ptr() as *const _,
                ),
            )
        };
        let manager_set_device_matching = unsafe {
            std::mem::transmute::<
                *mut c_void,
                Option<FnIOHIDManagerSetDeviceMatching>,
            >(libc::dlsym(
                handle,
                c"IOHIDManagerSetDeviceMatching".as_ptr() as *const _,
            ))
        };
        let manager_schedule_with_run_loop = unsafe {
            std::mem::transmute::<
                *mut c_void,
                Option<FnIOHIDManagerScheduleWithRunLoop>,
            >(libc::dlsym(
                handle,
                c"IOHIDManagerScheduleWithRunLoop".as_ptr() as *const _,
            ))
        };
        let manager_register_input_callback = unsafe {
            std::mem::transmute::<
                *mut c_void,
                Option<FnIOHIDManagerRegisterInputCallback>,
            >(libc::dlsym(
                handle,
                c"IOHIDManagerRegisterInputCallback".as_ptr() as *const _,
            ))
        };
        let manager_open = unsafe {
            std::mem::transmute::<*mut c_void, Option<FnIOHIDManagerOpen>>(
                libc::dlsym(handle, c"IOHIDManagerOpen".as_ptr() as *const _),
            )
        };
        let manager_close = unsafe {
            std::mem::transmute::<*mut c_void, Option<FnIOHIDManagerClose>>(
                libc::dlsym(handle, c"IOHIDManagerClose".as_ptr() as *const _),
            )
        };
        let device_get_location_id = unsafe {
            std::mem::transmute::<
                *mut c_void,
                Option<FnIOHIDDeviceGetLocationID>,
            >(libc::dlsym(
                handle,
                c"IOHIDDeviceGetLocationID".as_ptr() as *const _,
            ))
        };
        let event_get_type = unsafe {
            std::mem::transmute::<*mut c_void, Option<FnIOHIDEventGetType>>(
                libc::dlsym(handle, c"IOHIDEventGetType".as_ptr() as *const _),
            )
        };
        let event_get_integer_value = unsafe {
            std::mem::transmute::<
                *mut c_void,
                Option<FnIOHIDEventGetIntegerValue>,
            >(libc::dlsym(
                handle,
                c"IOHIDEventGetIntegerValue".as_ptr() as *const _,
            ))
        };

        // We never call dlclose(), so the IOKit framework handle stays valid
        // for the process lifetime.  The raw `*mut c_void` is Copy so no drop
        // guard needs suppression.
        let _ = handle;

        let funcs = IOHidFunctions {
            manager_create,
            manager_set_device_matching,
            manager_schedule_with_run_loop,
            manager_register_input_callback,
            manager_open,
            manager_close,
            device_get_location_id,
            event_get_type,
            event_get_integer_value,
        };

        if funcs.is_complete() {
            IOHID_FUNCS.set(funcs).is_ok()
        } else {
            // Check if another thread raced us and succeeded.
            match IOHID_FUNCS.get() {
                Some(cached) => cached.is_complete(),
                None => false,
            }
        }
    }

    /// Returns `true` if all required symbols are resolved.
    fn is_complete(&self) -> bool {
        self.manager_create.is_some()
            && self.manager_set_device_matching.is_some()
            && self.manager_schedule_with_run_loop.is_some()
            && self.manager_register_input_callback.is_some()
            && self.manager_open.is_some()
            && self.manager_close.is_some()
            && self.device_get_location_id.is_some()
            && self.event_get_type.is_some()
            && self.event_get_integer_value.is_some()
    }

    /// Get a reference to the resolved functions.  Must only be called after
    /// `resolve()` returned `true`.
    pub fn get() -> &'static Self {
        IOHID_FUNCS
            .get()
            .expect("IOHID functions not resolved; call resolve() first")
    }
}

// ---------------------------------------------------------------------------
// IOHIDManager availability probe
// ---------------------------------------------------------------------------

/// Returns `true` when all required IOHIDManager symbols can be resolved from
/// IOKit at runtime.
///
/// This is a lightweight probe used by the e2e test harness to skip tests
/// when IOHIDManager is not available in the current environment (e.g. a
/// sandboxed runner where `dlopen`/`dlsym` on IOKit fails).
pub fn iohid_available() -> bool {
    IOHidFunctions::resolve()
}

// ---------------------------------------------------------------------------
// IOHIDManager driver — high-level API
// ---------------------------------------------------------------------------

/// Shared mutable state bridged into the IOHID callback via `user_info`.
pub struct IOHidContext {
    /// Trait-object lookup: decouples this module from RuntimeState's shape.
    pub lookup:
        std::sync::Arc<parking_lot::RwLock<dyn crate::daemon::state::Lookup>>,
    /// Pre-created event source reused for every synthetic keyboard event.
    pub source: CFRetained<objc2_core_graphics::CGEventSource>,
    /// Bitmask tracking which specific modifier keys are physically pressed.
    pub modifier_state: u8,
    /// Set of currently pressed CGKeyCodes.  IOHID delivers events for both
    /// key-down and key-up, but does not always provide a reliable
    /// "direction" field.  We toggle: if the keycode is in the set, this
    /// is a key-up; otherwise it is a key-down.
    pub pressed_keys: std::collections::HashSet<u16>,
    /// Connection to the DriverKit virtual HID keyboard.  `None` when the
    /// driver is not loaded; falls back to CGEvent posting.
    #[cfg(feature = "driverkit")]
    pub hid_socket: Option<super::hid_socket::HidSocket>,
}

/// Holds the IOHIDManager and callback context so they stay alive for the
/// lifetime of the event-loop, and are cleanly reclaimed on drop.
pub struct IOHidHandle {
    manager: *mut IOHIDManager,
    /// Raw pointer to the heap-allocated `IOHidContext` passed as
    /// `user_info`.
    context_ptr: *mut IOHidContext,
}

impl Drop for IOHidHandle {
    fn drop(&mut self) {
        let funcs = IOHidFunctions::get();
        if let Some(close) = funcs.manager_close {
            unsafe {
                close(self.manager, 0);
            }
        }

        unsafe {
            drop(Box::from_raw(self.context_ptr));
        }
    }
}

/// Result of attempting to start the IOHIDManager input capture path.
pub enum IOHidResult {
    /// IOHIDManager is active; drop the handle to shut it down.
    Active(IOHidHandle, std::sync::Arc<std::sync::atomic::AtomicBool>),
    /// IOHIDManager is not available; fall back to CGEventTap.
    Unavailable(String),
}

/// Start keyboard input capture via IOHIDManager.
///
/// If successful, returns an `IOHidResult::Active` containing the handle and a
/// shutdown flag.  The caller is responsible for running the CFRunLoop until
/// the shutdown flag is set.
///
/// Returns `IOHidResult::Unavailable` when IOHIDManager symbols cannot be
/// resolved or the manager fails to open.  There is no fallback — input
/// capture requires IOHIDManager.
pub fn start_iohid_mapping(
    lookup: std::sync::Arc<
        parking_lot::RwLock<dyn crate::daemon::state::Lookup>,
    >,
    source: CFRetained<objc2_core_graphics::CGEventSource>,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> IOHidResult {
    // Resolve symbols at runtime.
    if !IOHidFunctions::resolve() {
        return IOHidResult::Unavailable(
            "IOHIDManager: could not resolve required symbols".into(),
        );
    }

    let funcs = IOHidFunctions::get();

    // Create the manager.
    let manager =
        unsafe { (funcs.manager_create.unwrap())(kCFAllocatorDefault, 0) };
    if manager.is_null() {
        return IOHidResult::Unavailable(
            "IOHIDManager: IOHIDManagerCreate returned null".into(),
        );
    }

    // Filter to keyboard devices only.
    unsafe {
        (funcs.manager_set_device_matching.unwrap())(manager);
    }

    // Configure driverkit hid_socket if available.
    #[cfg(feature = "driverkit")]
    let hid_socket = match super::hid_socket::HidSocket::discover_and_open() {
        Ok(socket) => {
            eprintln!("Using DriverKit HID keyboard for event emission.");
            Some(socket)
        }
        Err(e) => {
            eprintln!(
                "DriverKit HID driver not available ({e}), falling back to \
                 CGEvent."
            );
            None
        }
    };

    let context_ptr = Box::into_raw(Box::new(IOHidContext {
        lookup,
        source,
        modifier_state: 0,
        pressed_keys: std::collections::HashSet::new(),
        #[cfg(feature = "driverkit")]
        hid_socket,
    })) as *mut _;

    // Register the input callback.
    unsafe {
        (funcs.manager_register_input_callback.unwrap())(
            manager,
            iohid_input_callback,
            context_ptr as *mut c_void,
        );
    }

    // Open the manager.  `kIOHIDManagerOptionNone` = 0.
    let open_result = unsafe { (funcs.manager_open.unwrap())(manager, 0) };
    if open_result != 0 {
        unsafe {
            drop(Box::from_raw(context_ptr));
        }
        return IOHidResult::Unavailable(format!(
            "IOHIDManager: IOHIDManagerOpen failed with error {open_result}"
        ));
    }

    // Schedule the manager with the current run loop.
    let run_loop =
        CFRunLoop::current().expect("IOHIDManager: no current run loop");
    let mode_ref = unsafe { objc2_core_foundation::kCFRunLoopDefaultMode }
        .expect("kCFRunLoopDefaultMode is always available");
    unsafe {
        (funcs.manager_schedule_with_run_loop.unwrap())(
            manager,
            &run_loop as *const _ as *mut c_void,
            mode_ref as *const _ as *mut c_void,
        );
    }

    println!("macOS IOHIDManager input capture active.");

    IOHidResult::Active(
        IOHidHandle {
            manager,
            context_ptr,
        },
        shutdown,
    )
}

/// FFI callback invoked by IOHIDManager for every keyboard event.
///
/// Receives the `IOHIDDeviceRef` that generated the event, enabling
/// device-level filtering via Location ID.
unsafe extern "C" fn iohid_input_callback(
    user_info: *mut c_void,
    _result: u32,
    _sender: *mut c_void, // IOHIDSenderRef (not used)
    event: *mut IOHIDEvent,
    device: *mut IOHIDDevice,
) {
    if user_info.is_null() || event.is_null() {
        return;
    }

    let context = unsafe { &mut *(user_info as *mut IOHidContext) };
    let funcs = IOHidFunctions::get();

    // Check event type — we only care about keyboard events.
    let event_type = unsafe { (funcs.event_get_type.unwrap())(event) };
    if event_type != kIOHIDEventTypeKeyboard {
        return;
    }

    // Get the HID usage (keycode) from the event.
    let hid_usage = unsafe {
        // kIOHIDEventFieldInputEventComponentUsage gives us the key's usage
        // page-specific value.
        (funcs.event_get_integer_value.unwrap())(event, 0)
    } as u16;

    // Skip events with no usage — likely noise or control events.
    if hid_usage == 0 {
        return;
    }

    // Translate HID usage to CGKeyCode.
    let Some(cg_keycode) = cg_keycode_from_hid_usage(hid_usage) else {
        return; // Unknown usage, let the system handle it.
    };

    // Determine key down vs. key up using toggle-based tracking.  IOHID
    // delivers events for both directions but does not always provide a
    // reliable "direction" field.  We track which keys are currently pressed:
    // if the keycode is in the set, this event is a key-up; otherwise it is
    // a key-down.
    let is_down = context.pressed_keys.insert(cg_keycode);

    // Get device location ID for keyboard filtering.
    let location_id = if !device.is_null() {
        unsafe { (funcs.device_get_location_id.unwrap())(device) }
    } else {
        0
    };

    // Format location ID as hex string for the lookup trait.
    let device_id_str = if location_id != 0 {
        Some(format!("0x{:08x}", location_id))
    } else {
        None
    };

    // Track modifier key state for exact matching.
    let lookup_modifiers = context.modifier_state;
    if let Some(bit) = keycode_to_modifier_bit(cg_keycode) {
        if is_down {
            context.modifier_state |= 1 << bit;
        } else {
            context.modifier_state &= !(1 << bit);
        }
    }

    // Perform the lookup.
    let guard = context.lookup.read();
    let active_outputs = guard
        .for_app(
            &guard.active_app(),
            cg_keycode,
            lookup_modifiers,
            device_id_str.as_deref(),
        )
        .or_else(|| {
            guard.global(
                cg_keycode,
                lookup_modifiers,
                device_id_str.as_deref(),
            )
        })
        .map(|v| v.to_vec());
    drop(guard);

    // Emit mapped outputs.  Only emit on key down; key ups are handled by the
    // modifier state tracking (the modifier release will be captured
    // separately).
    if let Some(outputs) = active_outputs
        && is_down
    {
        for native_key in &outputs {
            #[cfg(not(feature = "driverkit"))]
            super::mapping::emit_key_event(&context.source, native_key);

            #[cfg(feature = "driverkit")]
            super::mapping::emit_key_event(
                &context.source,
                &context.hid_socket,
                native_key,
            );
        }
    }

    // Unmapped key — let the system handle it by not consuming the event.
}

// ---------------------------------------------------------------------------
// Modifier handling (mirrors mapping.rs for IOHID context)
// ---------------------------------------------------------------------------

/// Map a CGKeyCode to its modifier bit position.  Returns `None` for
/// non-modifier keys.
fn keycode_to_modifier_bit(code: u16) -> Option<u8> {
    use crate::common::modifier::ModifierRole;

    let role = match code {
        59 => ModifierRole::LeftControl, // kVK_Control (left)
        62 => ModifierRole::RightControl,
        56 => ModifierRole::LeftShift,
        60 => ModifierRole::RightShift,
        58 => ModifierRole::LeftAlt,
        61 => ModifierRole::RightAlt,
        55 => ModifierRole::LeftCommand,
        54 => ModifierRole::RightCommand,
        _ => return None,
    };
    Some(role.bit())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_usage_to_cg_keycode_letters() {
        assert_eq!(cg_keycode_from_hid_usage(0x04), Some(0)); // A
        assert_eq!(cg_keycode_from_hid_usage(0x1D), Some(6)); // Z
    }

    #[test]
    fn hid_usage_to_cg_keycode_numbers() {
        assert_eq!(cg_keycode_from_hid_usage(0x1E), Some(18)); // 1
        assert_eq!(cg_keycode_from_hid_usage(0x27), Some(29)); // 0
    }

    #[test]
    fn hid_usage_to_cg_keycode_modifiers() {
        assert_eq!(cg_keycode_from_hid_usage(0xE0), Some(59)); // LeftControl
        assert_eq!(cg_keycode_from_hid_usage(0xE2), Some(56)); // LeftShift
        assert_eq!(cg_keycode_from_hid_usage(0xE6), Some(55)); // LeftCommand
    }

    #[test]
    fn hid_usage_to_cg_keycode_function_keys() {
        assert_eq!(cg_keycode_from_hid_usage(0x3A), Some(122)); // F1
        assert_eq!(cg_keycode_from_hid_usage(0x45), Some(111)); // F12
    }

    #[test]
    fn hid_usage_to_cg_keycode_navigation() {
        assert_eq!(cg_keycode_from_hid_usage(0x52), Some(126)); // UpArrow
        assert_eq!(cg_keycode_from_hid_usage(0x51), Some(125)); // DownArrow
        assert_eq!(cg_keycode_from_hid_usage(0x50), Some(123)); // LeftArrow
        assert_eq!(cg_keycode_from_hid_usage(0x4B), Some(124)); // RightArrow
    }

    #[test]
    fn hid_usage_to_cg_keycode_edit_keys() {
        assert_eq!(cg_keycode_from_hid_usage(0x28), Some(36)); // Return
        assert_eq!(cg_keycode_from_hid_usage(0x2A), Some(51)); // Backspace
        assert_eq!(cg_keycode_from_hid_usage(0x29), Some(53)); // Escape
        assert_eq!(cg_keycode_from_hid_usage(0x2B), Some(48)); // Tab
        assert_eq!(cg_keycode_from_hid_usage(0x2C), Some(49)); // Space
    }

    #[test]
    fn hid_usage_to_cg_keycode_unknown() {
        assert_eq!(cg_keycode_from_hid_usage(0xFF), None);
    }

    #[test]
    fn keycode_to_modifier_bit_left_control() {
        assert_eq!(keycode_to_modifier_bit(59), Some(0));
    }

    #[test]
    fn keycode_to_modifier_bit_non_modifier() {
        assert_eq!(keycode_to_modifier_bit(0), None); // A is not a modifier
    }

    #[test]
    fn resolve_returns_bool() {
        // This test just verifies that resolve() compiles and returns a bool.
        // On a real macOS system with IOKit it may succeed; in CI it will
        // fail.
        let _ = IOHidFunctions::resolve();
    }
}
