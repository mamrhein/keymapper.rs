// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! IOKit HID device seizure for keyboard input capture on macOS.
//!
//! Follows the Karabiner Elements approach: uses `IOHIDManager` exclusively
//! for device discovery, then opens individual devices with
//! `kIOHIDOptionsTypeSeizeDevice` and captures events via per-device
//! `IOHIDQueue` callbacks.  This avoids the entitlement-gated
//! `IOHIDManagerRegisterInputCallback` API entirely.
//!
//! Requires root privileges for device seizure and the Input Monitoring
//! permission in System Settings.
#![allow(dead_code)]

use std::{ffi::c_void, ptr};

use objc2_core_foundation::{CFRetained, CFRunLoop, kCFRunLoopDefaultMode};

// ---------------------------------------------------------------------------
// Opaque IOKit HID types
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

/// Opaque `IOHIDQueueRef`.
#[repr(C)]
pub struct IOHIDQueue {
    _private: [u8; 0],
}

/// Opaque `IOHIDElementRef`.
#[repr(C)]
pub struct IOHIDElement {
    _private: [u8; 0],
}

/// Opaque `IOHIDValueRef`.
#[repr(C)]
pub struct IOHIDValue {
    _private: [u8; 0],
}

/// Opaque `CFSetRef` (returned by `IOHIDManagerCopyDevices`).
#[allow(non_camel_case_types)]
type CFSetRef = *mut c_void;

/// Opaque `CFTypeRef` (iterator element from `CFSet`).
#[allow(non_camel_case_types)]
type CFTypeRef = *const c_void;

/// Opaque `CFAllocatorRef` (we use null for `kCFAllocatorDefault`).
#[allow(non_camel_case_types)]
type CFAllocatorRef = *mut c_void;

/// Opaque `CFDictionaryRef`.
#[allow(non_camel_case_types)]
type CFDictionaryRef = *const c_void;

/// Opaque `CFNumberRef`.
#[allow(non_camel_case_types)]
type CFNumberRef = *const c_void;

/// Opaque `CFStringRef`.
#[allow(non_camel_case_types)]
type CFStringRef = *const c_void;

/// `kCFAllocatorDefault` is represented as NULL.
#[allow(non_upper_case_globals)]
const kCFAllocatorDefault: CFAllocatorRef = ptr::null_mut();

/// `kIOHIDOptionsTypeNone`.
#[allow(non_upper_case_globals)]
const kIOHIDOptionsTypeNone: u32 = 0;

/// `kIOHIDOptionsTypeSeizeDevice` — exclusive access to the device.
#[allow(non_upper_case_globals)]
const kIOHIDOptionsTypeSeizeDevice: u32 = 1;

/// `kIOHIDMapKeyLocationID`.
#[allow(non_upper_case_globals)]
const kIOHIDMapKeyLocationID: &str = "Location ID";

/// `kIOHIDMapKeyVendorID`.
#[allow(non_upper_case_globals)]
const kIOHIDMapKeyVendorID: &str = "Vendor ID";

/// `kIOHIDMapKeyProductID`.
#[allow(non_upper_case_globals)]
const kIOHIDMapKeyProductID: &str = "Product ID";

/// `kIOHIDMapKeyRegistryEntryID`.
#[allow(non_upper_case_globals)]
const kIOHIDMapKeyRegistryEntryID: &str = "Registry Entry ID";

/// USB HID usage page for Keyboard/Keypad.
const HID_USAGE_PAGE_KEYBOARD: u32 = 0x07;

// ---------------------------------------------------------------------------
// IOReturn constants
// ---------------------------------------------------------------------------

/// `kIOReturnSuccess`.
#[allow(non_upper_case_globals)]
const kIOReturnSuccess: u32 = 0;

/// `kIOReturnNotPermitted`.
#[allow(non_upper_case_globals)]
const kIOReturnNotPermitted: u32 = 0xe00002c7;

/// `kIOReturnExclusiveAccess`.
#[allow(non_upper_case_globals)]
const kIOReturnExclusiveAccess: u32 = 0xe00002b7;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors from IOKit HID operations.
#[derive(Debug)]
pub enum IoKitError {
    // The operation requires root privileges.
    NotPermitted(String),
    // The device is already seized by another process.
    ExclusiveAccess(String),
    // Generic IOKit error with the raw return code.
    IoReturn(u32, String),
    // A required FFI symbol could not be resolved.
    SymbolMissing(String),
}

impl std::fmt::Display for IoKitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IoKitError::NotPermitted(ctx) => {
                write!(
                    f,
                    "IOKit operation not permitted: {}. Run as root and \
                     grant Input Monitoring permission.",
                    ctx,
                )
            }
            IoKitError::ExclusiveAccess(ctx) => {
                write!(
                    f,
                    "IOKit exclusive access denied: {}. Another process may \
                     have seized this device.",
                    ctx,
                )
            }
            IoKitError::IoReturn(code, ctx) => {
                write!(f, "IOKit error 0x{:08x}: {}", code, ctx)
            }
            IoKitError::SymbolMissing(sym) => {
                write!(f, "IOKit symbol not found: {}", sym)
            }
        }
    }
}

impl std::error::Error for IoKitError {}

/// Convert an `IOReturn` to a result, mapping known error codes.
fn check_io_return(result: u32, context: &str) -> Result<(), IoKitError> {
    if result == kIOReturnSuccess {
        Ok(())
    } else if result == kIOReturnNotPermitted {
        Err(IoKitError::NotPermitted(context.to_string()))
    } else if result == kIOReturnExclusiveAccess {
        Err(IoKitError::ExclusiveAccess(context.to_string()))
    } else {
        Err(IoKitError::IoReturn(result, context.to_string()))
    }
}

// ---------------------------------------------------------------------------
// HID usage → CGKeyCode translation (reused from ioh_device.rs)
// ---------------------------------------------------------------------------

/// Translate a USB HID Keyboard/Keypad usage code to a macOS CGKeyCode.
///
/// Returns `None` for usages that have no known CGKeyCode equivalent.
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

        // --- Edit keys ---
        0x28 => 36,  // Return
        0x29 => 53,  // Escape
        0x2A => 51,  // Delete (Backspace)
        0x2B => 48,  // Tab
        0x2C => 49,  // Spacebar
        0x30 => 119, // Non-US Backslash

        // --- Modifiers ---
        0xE0 => 59, // LeftControl
        0xE1 => 62, // RightControl
        0xE2 => 56, // LeftShift
        0xE3 => 60, // RightShift
        0xE4 => 58, // LeftAlt (LeftOption)
        0xE5 => 61, // RightAlt (RightOption)
        0xE6 => 55, // LeftCommand (LeftGui)
        0xE7 => 54, // RightCommand (RightGui)

        // --- Navigation ---
        0x4A => 124, // RightArrow (page up on mac)
        0x4B => 124, // RightArrow
        0x4C => 123, // LeftArrow
        0x4D => 125, // DownArrow
        0x4E => 126, // UpArrow

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

        // --- Keypad ---
        0x52 => 124, // Keypad RightArrow
        0x51 => 125, // Keypad DownArrow
        0x50 => 123, // Keypad LeftArrow
        0x53 => 126, // Keypad UpArrow

        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// FFI declarations — linked directly against IOKit.framework
// ---------------------------------------------------------------------------

// Link directly against IOKit.framework.  This avoids the "stubs-only" issue
// that occurs when loading IOKit via `dlopen`/`dlsym`.  The Karabiner
// approach relies on direct linking for all IOHIDLib symbols.
#[allow(non_camel_case_types)]
unsafe extern "C" {
    // --- IOHIDManager (device discovery) ---

    // Create a new IOHIDManager.
    fn IOHIDManagerCreate(
        allocator: CFAllocatorRef,
        options: u32,
    ) -> *mut IOHIDManager;

    // Configure the manager to match keyboard devices.
    fn IOHIDManagerSetDeviceMatching(manager: *mut IOHIDManager);

    // Get the set of matched devices.
    fn IOHIDManagerCopyDevices(manager: *mut IOHIDManager) -> CFSetRef;

    // Schedule the manager with a run loop (for hotplug support).
    fn IOHIDManagerScheduleWithRunLoop(
        manager: *mut IOHIDManager,
        run_loop: *mut c_void, // CFRunLoopRef
        mode: *mut c_void,     // CFStringRef
    );

    // Open the manager (required before CopyDevices returns usable refs).
    fn IOHIDManagerOpen(manager: *mut IOHIDManager, flags: u32) -> u32;

    // Close the manager.
    fn IOHIDManagerClose(manager: *mut IOHIDManager, flags: u32) -> u32;

    // --- IOHIDDevice ---

    // Open a HID device. Use `kIOHIDOptionsTypeSeizeDevice` for exclusive
    // access (Karabiner approach).
    fn IOHIDDeviceOpen(device: *mut IOHIDDevice, flags: u32) -> u32;

    // Close a HID device.
    fn IOHIDDeviceClose(device: *mut IOHIDDevice, flags: u32) -> u32;

    // Get the device's Location ID.
    fn IOHIDDeviceGetLocationID(device: *mut IOHIDDevice) -> u32;

    // Create a queue for receiving events from this device.
    fn IOHIDDeviceCreateQueue(
        device: *mut IOHIDDevice,
        allocator: CFAllocatorRef,
    ) -> *mut IOHIDQueue;

    // --- IOHIDQueue ---

    // Register a callback for value-available events on the queue.
    fn IOHIDQueueRegisterValueAvailableCallback(
        queue: *mut IOHIDQueue,
        callout: Option<
            unsafe extern "C" fn(
                context: *mut c_void,
                queue: *mut IOHIDQueue,
                _unused: u32,
                values: *mut c_void, // CFArrayRef of IOHIDValueRef
            ),
        >,
        context: *mut c_void,
    );

    // Schedule the queue with a run loop.
    fn IOHIDQueueScheduleWithRunLoop(
        queue: *mut IOHIDQueue,
        run_loop: *mut c_void, // CFRunLoopRef
        mode: *mut c_void,     // CFStringRef
    );

    // Open (start) the queue.
    fn IOHIDQueueOpen(queue: *mut IOHIDQueue) -> u32;

    // Close (stop) the queue.
    fn IOHIDQueueClose(queue: *mut IOHIDQueue, flags: u32) -> u32;

    // --- IOHIDValue / IOHIDElement ---

    // Get the integer value from an IOHIDValue.
    fn IOHIDValueGetInteger(value: *mut IOHIDValue) -> u32;

    // Get the element that produced this value.
    fn IOHIDValueGetElement(value: *mut IOHIDValue) -> *mut IOHIDElement;

    // Get the usage page of an element.
    fn IOHIDElementGetUsagePage(element: *mut IOHIDElement) -> u32;

    // Get the usage code of an element.
    fn IOHIDElementGetUsage(element: *mut IOHIDElement) -> u32;

    // --- CFArray (value list from queue callback) ---

    // Get the number of items in a CFArray.
    fn CFArrayGetCount(the_array: *const c_void) -> usize;

    // Get an item from a CFArray.
    fn CFArrayGetValueAtIndex(
        the_array: *const c_void,
        idx: usize,
    ) -> *const c_void;

    // --- CFSet (device set from CopyDevices) ---

    // Get the number of items in a CFSet.
    fn CFSetGetCount(the_set: CFSetRef) -> usize;

    // Get an iterator for a CFSet.
    fn CFSetCreateIterator(
        the_set: CFSetRef,
        iterator: *mut *mut c_void,
    ) -> bool;

    // Get the next value from a CFSet iterator.
    fn CFSetIteratorGetNext(iterator: *mut c_void) -> CFTypeRef;

    // Release a CF object.
    fn CFRelease(cf: *const c_void);

    // --- CFNumber (device properties) ---

    // Get a property from an IOHIDDevice as CFNumber.
    fn IOHIDDeviceCopyCFTypeArgumentByIndex(
        device: *mut IOHIDDevice,
        index: usize,
    ) -> *const c_void;

    // Create a CFNumber from u32.
    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: u32,
        value_ptr: *const c_void,
    ) -> CFNumberRef;

    // Get a u32 from a CFNumber.
    fn CFNumberGetValue(
        number: CFNumberRef,
        the_type: u32,
        value_ptr: *mut c_void,
    ) -> bool;

    // kCFNumberUIntType.
    static kCFNumberUIntType: u32;

    // kCFNumberInt32Type.
    static kCFNumberInt32Type: u32;

    // --- CFDictionary (device matching) ---

    // Create a mutable dictionary.
    fn CFDictionaryCreateMutable(
        allocator: CFAllocatorRef,
        capacity: isize,
        key_type: *const c_void,
        value_type: *const c_void,
    ) -> *mut c_void;

    // Set a value in the dictionary.
    fn CFDictionarySetValue(
        dict: *mut c_void,
        key: *const c_void,
        value: *const c_void,
    );

    // Set device matching with a custom dictionary.
    fn IOHIDManagerSetDeviceMatchingWithDictionary(
        manager: *mut IOHIDManager,
        dict: CFDictionaryRef,
    );
}

// ---------------------------------------------------------------------------
// HID input element type constant (kHIDElementTypeInput_Misc = 0)
// ---------------------------------------------------------------------------

/// `kHIDInputElementTypeInputMisc` — used for creating a queue that
/// receives all input events from the device.
#[allow(non_upper_case_globals)]
const kHIDInputElementTypeInputMisc: u32 = 0;

// ---------------------------------------------------------------------------
// HID value callback context
// ---------------------------------------------------------------------------

/// Context passed to the queue value-available callback.
pub struct HidQueueContext {
    // Shared lookup for remapping rules.
    pub lookup:
        std::sync::Arc<parking_lot::RwLock<dyn crate::daemon::state::Lookup>>,
    // Pre-created event source for synthetic keyboard events.
    pub source: CFRetained<objc2_core_graphics::CGEventSource>,
    // Bitmask tracking which modifier keys are physically pressed.
    pub modifier_state: u8,
    // Set of currently pressed keycodes for key-up toggling.
    pub pressed_keys: std::collections::HashSet<u16>,
    // Device location ID string for keyboard filtering.
    pub device_id: String,
    // Connection to the DriverKit virtual HID keyboard.
    #[cfg(feature = "driverkit")]
    pub hid_socket: Option<super::hid_socket::HidSocket>,
}

// ---------------------------------------------------------------------------
// HidDevice — represents a discovered HID keyboard device
// ---------------------------------------------------------------------------

/// A single HID keyboard device discovered via IOHIDManager.
///
/// Holds the `IOHIDDeviceRef` and pre-cached properties for filtering.
pub struct HidDevice {
    // Raw device reference.
    device: *mut IOHIDDevice,
    // Pre-cached location ID.
    location_id: u32,
}

impl HidDevice {
    // Open the device with the specified options.
    ///
    // Use `kIOHIDOptionsTypeSeizeDevice` for exclusive access (Karabiner
    // approach).  Requires root privileges.
    pub fn open(&self, seize: bool) -> Result<(), IoKitError> {
        let flags = if seize {
            kIOHIDOptionsTypeSeizeDevice
        } else {
            kIOHIDOptionsTypeNone
        };

        let result = unsafe { IOHIDDeviceOpen(self.device, flags) };
        check_io_return(
            result,
            &format!(
                "IOHIDDeviceOpen for device at location 0x{:08x}",
                self.location_id,
            ),
        )
    }

    // Close the device.
    pub fn close(&self) {
        unsafe { IOHIDDeviceClose(self.device, kIOHIDOptionsTypeNone); }
    }

    // Create an input queue for this device.
    pub fn create_queue(&self) -> Result<HidQueue, IoKitError> {
        let queue = unsafe {
            IOHIDDeviceCreateQueue(self.device, kCFAllocatorDefault)
        };

        if queue.is_null() {
            return Err(IoKitError::IoReturn(
                0xfffffff0, // kIOReturnBadArgument
                "IOHIDDeviceCreateQueue returned null".into(),
            ));
        }

        Ok(HidQueue { queue })
    }

    // Returns the location ID of this device.
    pub fn location_id(&self) -> u32 {
        self.location_id
    }

    // Returns the location ID as a hex string for the lookup trait.
    pub fn location_id_string(&self) -> String {
        format!("0x{:08x}", self.location_id)
    }

    // Returns the raw device reference.
    pub fn as_raw(&self) -> *mut IOHIDDevice {
        self.device
    }
}

// ---------------------------------------------------------------------------
// HidQueue — receives HID values from a seized device
// ---------------------------------------------------------------------------

/// An `IOHIDQueue` that receives raw HID values from a seized device.
pub struct HidQueue {
    queue: *mut IOHIDQueue,
}

impl HidQueue {
    // Register a callback for HID value events.
    ///
    // The context will be leaked and freed when the queue is dropped.  This
    // is safe because the queue outlives the context in normal operation, and
    // we free it explicitly on drop.
    pub fn register_value_callback(
        &self,
        context: HidQueueContext,
    ) -> HidQueueHandle {
        let context_ptr = Box::into_raw(Box::new(context));

        unsafe {
            IOHIDQueueRegisterValueAvailableCallback(
                self.queue,
                Some(hid_queue_value_callback),
                context_ptr as *mut c_void,
            );
        }

        HidQueueHandle {
            queue: self.queue,
            context_ptr: context_ptr as *mut c_void,
        }
    }

    // Schedule the queue with the current run loop.
    pub fn schedule_with_runloop(&self) {
        let run_loop =
            CFRunLoop::current().expect("IOHIDQueue: no current run loop");
        let mode_ref = unsafe { kCFRunLoopDefaultMode }
            .expect("kCFRunLoopDefaultMode is always available");

        unsafe {
            IOHIDQueueScheduleWithRunLoop(
                self.queue,
                &run_loop as *const _ as *mut c_void,
                mode_ref as *const _ as *mut c_void,
            );
        }
    }

    // Open (start) the queue.  Must be called after registering callbacks.
    pub fn open(&self) -> Result<(), IoKitError> {
        let result = unsafe { IOHIDQueueOpen(self.queue) };
        check_io_return(result, "IOHIDQueueOpen")
    }
}

/// FFI callback invoked by IOHIDQueue for every HID value event.
///
/// Each `values` argument is a CFArray of `IOHIDValueRef` pointers.  We
/// iterate the array and extract usage page, usage code, and value from
/// each element.
unsafe extern "C" fn hid_queue_value_callback(
    user_info: *mut c_void,
    _queue: *mut IOHIDQueue,
    _unused: u32,
    values: *mut c_void, // CFArrayRef of IOHIDValueRef
) {
    if user_info.is_null() || values.is_null() {
        return;
    }

    let context = unsafe { &mut *(user_info as *mut HidQueueContext) };

    // Iterate the CFArray of values.
    let count = unsafe { CFArrayGetCount(values as *const c_void) };

    for i in 0..count {
        let value_ref = unsafe {
            CFArrayGetValueAtIndex(values as *const c_void, i)
                as *mut IOHIDValue
        };

        if value_ref.is_null() {
            continue;
        }

        // Get the element that produced this value.
        let element = unsafe { IOHIDValueGetElement(value_ref) };
        if element.is_null() {
            continue;
        }

        // Extract usage page and usage code.
        let usage_page = unsafe { IOHIDElementGetUsagePage(element) };
        let usage = unsafe { IOHIDElementGetUsage(element) } as u16;

        // Skip non-keyboard events.
        if usage_page != HID_USAGE_PAGE_KEYBOARD {
            continue;
        }

        // Get the value (0 = up, non-zero = down).
        let raw_value = unsafe { IOHIDValueGetInteger(value_ref) };
        let is_down = raw_value != 0;

        // Translate HID usage to CGKeyCode.
        let Some(cg_keycode) = cg_keycode_from_hid_usage(usage) else {
            continue;
        };

        // Determine key down vs. key up.  IOHIDQueue delivers explicit
        // direction via the value field (unlike the manager-level callback
        // which requires toggle tracking).  We still maintain the set for
        // idempotency.
        let actually_down = context.pressed_keys.insert(cg_keycode);
        if !is_down {
            context.pressed_keys.remove(&cg_keycode);
        }

        // Only process key-down events for remapping.
        if !is_down || !actually_down {
            continue;
        }

        // Get the device ID for keyboard filtering.
        let device_id = Some(context.device_id.as_str());

        // Track modifier state.
        let lookup_modifiers = context.modifier_state;
        if let Some(bit) = keycode_to_modifier_bit(cg_keycode) {
            context.modifier_state |= 1 << bit;
        }

        // Perform the lookup.
        let guard = context.lookup.read();
        let active_outputs = guard
            .for_app(
                &guard.active_app(),
                cg_keycode,
                lookup_modifiers,
                device_id,
            )
            .or_else(|| guard.global(cg_keycode, lookup_modifiers, device_id))
            .map(|v| v.to_vec());
        drop(guard);

        // Emit mapped outputs.
        if let Some(outputs) = active_outputs {
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
    }
}

/// Handle that keeps a queue and its context alive, and cleans up on drop.
pub struct HidQueueHandle {
    queue: *mut IOHIDQueue,
    context_ptr: *mut c_void,
}

impl Drop for HidQueueHandle {
    fn drop(&mut self) {
        unsafe {
            IOHIDQueueClose(self.queue, kIOHIDOptionsTypeNone);
            drop(Box::from_raw(self.context_ptr as *mut HidQueueContext));
        }
    }
}

// ---------------------------------------------------------------------------
// HidDeviceManager — discovers keyboards via IOHIDManager (matching only)
// ---------------------------------------------------------------------------

/// Device discovery via `IOHIDManager`.  Used exclusively for matching
/// devices; does NOT register input callbacks on the manager.  Instead,
/// individual devices are opened directly and captured via `IOHIDQueue`.
pub struct HidDeviceManager {
    manager: *mut IOHIDManager,
}

impl HidDeviceManager {
    // Create a new manager configured to match keyboard devices.
    pub fn new_keyboard_matcher() -> Result<Self, IoKitError> {
        let manager = unsafe {
            IOHIDManagerCreate(kCFAllocatorDefault, kIOHIDOptionsTypeNone)
        };

        if manager.is_null() {
            return Err(IoKitError::IoReturn(
                0xfffffff0,
                "IOHIDManagerCreate returned null".into(),
            ));
        }

        // Build a custom matching dictionary for keyboards.  We match on
        // primary usage page (0x01) and primary usage (0x06 = keyboard).
        let dict = unsafe {
            CFDictionaryCreateMutable(
                kCFAllocatorDefault,
                0,
                ptr::null(),
                ptr::null(),
            )
        };

        // kHIDPrimaryUsagePage = 0x01.
        let usage_page_number = unsafe {
            CFNumberCreate(
                kCFAllocatorDefault,
                kCFNumberUIntType,
                &0x01u32 as *const _ as *const _,
            )
        };
        // "Primary Usage Page" key.
        let usage_page_key = create_cf_string("Primary Usage Page");
        unsafe {
            CFDictionarySetValue(
                dict,
                usage_page_key as *const _,
                usage_page_number as *const _,
            );
        }

        // kHIDPrimaryUsage = 0x06 (keyboard).
        let usage_number = unsafe {
            CFNumberCreate(
                kCFAllocatorDefault,
                kCFNumberUIntType,
                &0x06u32 as *const _ as *const _,
            )
        };
        let usage_key = create_cf_string("Primary Usage");
        unsafe {
            CFDictionarySetValue(
                dict,
                usage_key as *const _,
                usage_number as *const _,
            );
        }

        // Apply the matching dictionary.
        unsafe {
            IOHIDManagerSetDeviceMatchingWithDictionary(manager, dict);
        }

        // Release CF objects.
        unsafe {
            CFRelease(dict as *const _);
            CFRelease(usage_page_key as *const _);
            CFRelease(usage_page_number as *const _);
            CFRelease(usage_key as *const _);
            CFRelease(usage_number as *const _);
        }

        let mgr = HidDeviceManager { manager };

        // Open the manager before scanning.
        mgr.open()?;

        Ok(mgr)
    }

    // Open the manager (required before scanning devices).
    fn open(&self) -> Result<(), IoKitError> {
        let result = unsafe { IOHIDManagerOpen(self.manager, 0) };
        check_io_return(result, "IOHIDManagerOpen")
    }

    // Synchronously scan for connected keyboard devices.
    pub fn scan_devices(&self) -> Vec<HidDevice> {
        let device_set = unsafe { IOHIDManagerCopyDevices(self.manager) };

        if device_set.is_null() {
            return Vec::new();
        }

        let mut devices = Vec::new();

        // Iterate the CFSet of matched devices.
        let mut iterator: *mut c_void = ptr::null_mut();
        let has_iterator =
            unsafe { CFSetCreateIterator(device_set, &mut iterator) };

        if has_iterator {
            loop {
                let device_ptr = unsafe { CFSetIteratorGetNext(iterator) };
                if device_ptr.is_null() {
                    break;
                }

                let device = device_ptr as *mut IOHIDDevice;

                // Get the location ID.
                let location_id = unsafe { IOHIDDeviceGetLocationID(device) };

                devices.push(HidDevice {
                    device,
                    location_id,
                });
            }

            unsafe { CFRelease(iterator as *const _) };
        }

        // Release the CFSet.
        unsafe { CFRelease(device_set as *const _) };

        devices
    }

    // Schedule the manager with the current run loop for hotplug support.
    pub fn schedule_with_runloop(&self) {
        let run_loop = CFRunLoop::current()
            .expect("HidDeviceManager: no current run loop");
        let mode_ref = unsafe { kCFRunLoopDefaultMode }
            .expect("kCFRunLoopDefaultMode is always available");

        unsafe {
            IOHIDManagerScheduleWithRunLoop(
                self.manager,
                &run_loop as *const _ as *mut c_void,
                mode_ref as *const _ as *mut c_void,
            );
        }
    }

    // Returns true if the manager is valid.
    pub fn is_valid(&self) -> bool {
        !self.manager.is_null()
    }
}

impl Drop for HidDeviceManager {
    fn drop(&mut self) {
        if !self.manager.is_null() {
            unsafe {
                IOHIDManagerClose(self.manager, kIOHIDOptionsTypeNone);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: create a CFString from &str (leaked, caller must CFRelease)
// ---------------------------------------------------------------------------

/// Create a `CFString` from a Rust string slice.  The returned pointer
/// must be released with `CFRelease` when no longer needed.
fn create_cf_string(s: &str) -> CFStringRef {

    use objc2_foundation::NSString;

    let ns = NSString::from_str(s);
    // CFString and NSString are toll-free bridged.  We transfer ownership
    // to the caller via a raw pointer that they must CFRelease.
    let ptr: *const objc2_foundation::NSString = ns.as_ref();
    ptr as CFStringRef
}

// ---------------------------------------------------------------------------
// Modifier handling (mirrors mapping.rs for IOHID context)
// ---------------------------------------------------------------------------

/// Map a CGKeyCode to its modifier bit position.  Returns `None` for
/// non-modifier keys.
fn keycode_to_modifier_bit(code: u16) -> Option<u8> {
    use crate::common::modifier::ModifierRole;

    let role = match code {
        59 => ModifierRole::LeftControl,
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
// Public: start IOHID device-seizure based mapping
// ---------------------------------------------------------------------------

/// Start keyboard input capture via IOKit device seizure.
///
/// This follows the Karabiner Elements approach:
/// 1. Discover keyboards via IOHIDManager (matching only).
/// 2. Open each device with `kIOHIDOptionsTypeSeizeDevice`.
/// 3. Create an IOHIDQueue for each device and register a value callback.
/// 4. Run the CFRunLoop to receive events.
///
/// Requires root privileges and Input Monitoring permission.
pub fn start_iohid_seizure_mapping(
    lookup: std::sync::Arc<
        parking_lot::RwLock<dyn crate::daemon::state::Lookup>,
    >,
) -> Result<SeizureHandle, IoKitError> {
    // Discover physical keyboards.
    let manager = HidDeviceManager::new_keyboard_matcher()?;
    let devices = manager.scan_devices();

    if devices.is_empty() {
        return Err(IoKitError::IoReturn(
            0,
            "No keyboard devices found via IOHIDManager".into(),
        ));
    }

    println!("IOKit HID: discovered {} keyboard device(s)", devices.len(),);

    // Create the event source for output emission.
    let source = objc2_core_graphics::CGEventSource::new(
        objc2_core_graphics::CGEventSourceStateID::CombinedSessionState,
    )
    .ok_or(IoKitError::IoReturn(
        0,
        "Failed to create CGEventSource".into(),
    ))?;

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
                 CGEvent.",
            );
            None
        }
    };

    // Open and seize each device, creating queues.
    let mut queue_handles = Vec::new();

    for device in &devices {
        let device_id = device.location_id_string();
        println!("IOKit HID: seizing device at location {}", device_id,);

        // Seize the device.
        device.open(true).map_err(|e| {
            eprintln!(
                "IOKit HID: failed to seize device {}: {}",
                device_id, e
            );
            e
        })?;

        // Create a queue and register the callback.
        let queue = device.create_queue()?;

        // Build the context for this device's callback.
        let ctx = HidQueueContext {
            lookup: lookup.clone(),
            source: source.clone(),
            modifier_state: 0,
            pressed_keys: std::collections::HashSet::new(),
            device_id,
            #[cfg(feature = "driverkit")]
            hid_socket: None, /* Each device gets its own socket reference
                               * if needed. */
        };

        let handle = queue.register_value_callback(ctx);

        // Schedule and open the queue.
        queue.schedule_with_runloop();
        queue.open()?;

        println!(
            "IOKit HID: queue active for device {}",
            device.location_id_string()
        );

        queue_handles.push(handle);
    }

    // Schedule the manager for hotplug.
    manager.schedule_with_runloop();

    Ok(SeizureHandle {
        _manager: manager,
        _devices: devices,
        _queue_handles: queue_handles,
    })
}

/// Handle that keeps all seized devices and queues alive.  Drop to release.
pub struct SeizureHandle {
    _manager: HidDeviceManager,
    _devices: Vec<HidDevice>,
    _queue_handles: Vec<HidQueueHandle>,
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
        assert_eq!(cg_keycode_from_hid_usage(0x52), Some(124)); // UpArrow
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
    fn check_io_return_success() {
        assert!(check_io_return(kIOReturnSuccess, "test").is_ok());
    }

    #[test]
    fn check_io_return_not_permitted() {
        let err = check_io_return(kIOReturnNotPermitted, "test");
        assert!(matches!(err, Err(IoKitError::NotPermitted(_))));
    }

    #[test]
    fn check_io_return_exclusive_access() {
        let err = check_io_return(kIOReturnExclusiveAccess, "test");
        assert!(matches!(err, Err(IoKitError::ExclusiveAccess(_))));
    }

    #[test]
    fn check_io_return_generic() {
        let err = check_io_return(0xdeadbeef, "test");
        assert!(matches!(err, Err(IoKitError::IoReturn(0xdeadbeef, _))));
    }
}
