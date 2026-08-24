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
#![allow(dead_code, non_snake_case)]

use std::{
    collections::HashSet,
    ffi::c_void,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use objc2_core_foundation::{
    CFIndex, CFRunLoop, CFString, CFStringBuiltInEncodings,
    kCFRunLoopDefaultMode,
};

use crate::common::hid_usage::{HidUsage, PAGE_CONSUMER};

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

/// `kCFNumberUIntType` — unsigned 32-bit integer CFNumber type.
#[allow(non_upper_case_globals)]
const kCFNumberUIntType: u32 = 1;

/// `kCFNumberInt32Type` — signed 32-bit integer CFNumber type.
#[allow(non_upper_case_globals)]
const kCFNumberInt32Type: u32 = 3;

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

/// `kIOHIDSerialNumberKey`.
#[allow(non_upper_case_globals)]
const kIOHIDSerialNumberKey: &str = "Serial Number";

/// `kIOHIDProductKey`.
#[allow(non_upper_case_globals)]
const kIOHIDProductKey: &str = "Product";

/// Serial number of the Karabiner DriverKit virtual keyboard.
///
/// Hardcoded in the pqrs driver, so it is stable across versions.  This is
/// the primary marker used to exclude our own output device from seizure —
/// seizing it would create an infinite remap loop (feedback loop).
const KARABINER_VIRTUAL_KEYBOARD_SERIAL: &str =
    "pqrs.org:Karabiner-DriverKit-VirtualHIDKeyboard";

/// Product-name prefix of the Karabiner DriverKit virtual keyboard.
///
/// The full product string is `Karabiner DriverKit VirtualHIDKeyboard
/// <driver-version>`, so only a prefix match is possible.  Used as a
/// secondary check when the serial number property is unavailable.
const KARABINER_VIRTUAL_KEYBOARD_PRODUCT_PREFIX: &str =
    "Karabiner DriverKit VirtualHIDKeyboard";

/// USB HID usage page for Keyboard/Keypad.
const HID_USAGE_PAGE_KEYBOARD: u32 = 0x07;

/// USB HID usage page for Consumer.
const HID_USAGE_PAGE_CONSUMER: u32 = 0x0C;

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
// FFI declarations — resolved dynamically via dlsym
// ---------------------------------------------------------------------------

// On modern macOS with SIP, the IOKit framework is a "stub" — the actual
// IOHIDLib symbols are only accessible at runtime via dlopen/dlsym, not at
// link time.

/// Function pointer types for IOHIDLib symbols.
type FnIOHIDManagerCreate =
    unsafe extern "C" fn(CFAllocatorRef, u32) -> *mut IOHIDManager;
type FnIOHIDManagerSetDeviceMatching = unsafe extern "C" fn(*mut IOHIDManager);
type FnIOHIDManagerCopyDevices =
    unsafe extern "C" fn(*mut IOHIDManager) -> CFSetRef;
type FnIOHIDManagerScheduleWithRunLoop = unsafe extern "C" fn(
    *mut IOHIDManager,
    *mut c_void, // CFRunLoopRef
    *mut c_void, // CFStringRef
);
type FnIOHIDManagerOpen = unsafe extern "C" fn(*mut IOHIDManager, u32) -> u32;
type FnIOHIDManagerClose = unsafe extern "C" fn(*mut IOHIDManager, u32) -> u32;
type FnIOHIDDeviceOpen = unsafe extern "C" fn(*mut IOHIDDevice, u32) -> u32;
type FnIOHIDDeviceClose = unsafe extern "C" fn(*mut IOHIDDevice, u32) -> u32;
type FnIOHIDDeviceGetLocationID =
    unsafe extern "C" fn(*mut IOHIDDevice) -> u32;
type FnIOHIDDeviceGetProperty =
    unsafe extern "C" fn(*mut IOHIDDevice, CFStringRef) -> *const c_void;
type FnIOHIDDeviceCreateQueue =
    unsafe extern "C" fn(*mut IOHIDDevice, CFAllocatorRef) -> *mut IOHIDQueue;
type FnIOHIDQueueRegisterValueAvailableCallback = unsafe extern "C" fn(
    *mut IOHIDQueue,
    Option<
        unsafe extern "C" fn(
            *mut c_void,
            *mut IOHIDQueue,
            u32,
            *mut c_void, // CFArrayRef of IOHIDValueRef
        ),
    >,
    *mut c_void,
);
type FnIOHIDQueueScheduleWithRunLoop = unsafe extern "C" fn(
    *mut IOHIDQueue,
    *mut c_void, // CFRunLoopRef
    *mut c_void, // CFStringRef
);
type FnIOHIDQueueOpen = unsafe extern "C" fn(*mut IOHIDQueue) -> u32;
type FnIOHIDQueueClose = unsafe extern "C" fn(*mut IOHIDQueue, u32);
type FnIOHIDValueGetInteger = unsafe extern "C" fn(*mut IOHIDValue) -> u32;
type FnIOHIDValueGetElement =
    unsafe extern "C" fn(*mut IOHIDValue) -> *mut IOHIDElement;
type FnIOHIDElementGetUsagePage =
    unsafe extern "C" fn(*mut IOHIDElement) -> u32;
type FnIOHIDElementGetUsage = unsafe extern "C" fn(*mut IOHIDElement) -> u32;
type FnCFArrayGetCount = unsafe extern "C" fn(*const c_void) -> usize;
type FnCFArrayGetValueAtIndex =
    unsafe extern "C" fn(*const c_void, usize) -> *const c_void;
type FnCFSetGetCount = unsafe extern "C" fn(CFSetRef) -> usize;
type FnCFSetCreateIterator =
    unsafe extern "C" fn(CFSetRef, *mut *mut c_void) -> bool;
type FnCFSetIteratorGetNext = unsafe extern "C" fn(*mut c_void) -> CFTypeRef;
type FnCFRelease = unsafe extern "C" fn(*const c_void);
type FnIOHIDDeviceCopyCFTypeArgumentByIndex =
    unsafe extern "C" fn(*mut IOHIDDevice, usize) -> *const c_void;
type FnCFNumberCreate =
    unsafe extern "C" fn(CFAllocatorRef, u32, *const c_void) -> CFNumberRef;
#[allow(improper_ctypes_definitions)]
type FnCFNumberGetValue =
    unsafe extern "C" fn(CFNumberRef, u32, *mut c_void) -> bool;
type FnCFDictionaryCreateMutable = unsafe extern "C" fn(
    CFAllocatorRef,
    isize,
    *const c_void,
    *const c_void,
) -> *mut c_void;
type FnCFDictionarySetValue =
    unsafe extern "C" fn(*mut c_void, *const c_void, *const c_void);
type FnIOHIDManagerSetDeviceMatchingWithDictionary =
    unsafe extern "C" fn(*mut IOHIDManager, CFDictionaryRef);

/// Resolved function pointers for the IOHIDLib API.
static IOHID_FUNCS: std::sync::OnceLock<IoKitFunctions> =
    std::sync::OnceLock::new();

/// Holds all resolved IOHID function pointers.
struct IoKitFunctions {
    manager_create: FnIOHIDManagerCreate,
    manager_set_device_matching: FnIOHIDManagerSetDeviceMatching,
    manager_copy_devices: FnIOHIDManagerCopyDevices,
    manager_schedule_with_runloop: FnIOHIDManagerScheduleWithRunLoop,
    manager_open: FnIOHIDManagerOpen,
    manager_close: FnIOHIDManagerClose,
    device_open: FnIOHIDDeviceOpen,
    device_close: FnIOHIDDeviceClose,
    device_get_location_id: FnIOHIDDeviceGetLocationID,
    device_get_property: FnIOHIDDeviceGetProperty,
    device_create_queue: FnIOHIDDeviceCreateQueue,
    queue_register_callback: FnIOHIDQueueRegisterValueAvailableCallback,
    queue_schedule_with_runloop: FnIOHIDQueueScheduleWithRunLoop,
    queue_open: FnIOHIDQueueOpen,
    queue_close: FnIOHIDQueueClose,
    value_get_integer: FnIOHIDValueGetInteger,
    value_get_element: FnIOHIDValueGetElement,
    element_get_usage_page: FnIOHIDElementGetUsagePage,
    element_get_usage: FnIOHIDElementGetUsage,
    cf_array_get_count: FnCFArrayGetCount,
    cf_array_get_value_at_index: FnCFArrayGetValueAtIndex,
    cf_set_get_count: FnCFSetGetCount,
    cf_set_create_iterator: FnCFSetCreateIterator,
    cf_set_iterator_get_next: FnCFSetIteratorGetNext,
    cf_release: FnCFRelease,
    device_copy_cf_type_arg_by_index: FnIOHIDDeviceCopyCFTypeArgumentByIndex,
    cf_number_create: FnCFNumberCreate,
    cf_number_get_value: FnCFNumberGetValue,
    cf_dict_create_mutable: FnCFDictionaryCreateMutable,
    cf_dict_set_value: FnCFDictionarySetValue,
    manager_set_device_matching_with_dict:
        FnIOHIDManagerSetDeviceMatchingWithDictionary,
}

impl IoKitFunctions {
    /// Resolve all IOHIDLib symbols from IOKit at runtime.
    ///
    /// Returns `true` when all required symbols are available.
    /// Resolve all IOHIDLib symbols from IOKit at runtime.
    ///
    /// Returns `Ok(())` when all required symbols are available.
    fn resolve() -> Result<(), ()> {
        if IOHID_FUNCS.get().is_some() {
            return Ok(());
        }

        // Load the IOKit framework dynamically.
        let path = b"/System/Library/Frameworks/IOKit.framework/IOKit\0";
        let handle =
            unsafe { libc::dlopen(path.as_ptr() as *const _, libc::RTLD_NOW) };
        if handle.is_null() {
            return Err(());
        }

        // SAFETY: `Option<FnType>` uses niche optimization where null pointer
        // bits represent `None`.  Transmuting `*mut c_void` (from dlsym) to
        // `Option<FnType>` is valid because both have identical size and
        // alignment, and the null/non-null bit patterns match.
        macro_rules! resolve_sym {
            ($handle:expr, $name:expr, $ty:ty) => {{
                let raw = unsafe {
                    libc::dlsym($handle, $name.as_ptr() as *const _)
                };
                let opt: Option<$ty> = unsafe { std::mem::transmute(raw) };
                opt.ok_or(())
            }};
        }

        let funcs = IoKitFunctions {
            manager_create: resolve_sym!(
                handle,
                b"IOHIDManagerCreate\0",
                FnIOHIDManagerCreate
            )?,
            manager_set_device_matching: resolve_sym!(
                handle,
                b"IOHIDManagerSetDeviceMatching\0",
                FnIOHIDManagerSetDeviceMatching
            )?,
            manager_copy_devices: resolve_sym!(
                handle,
                b"IOHIDManagerCopyDevices\0",
                FnIOHIDManagerCopyDevices
            )?,
            manager_schedule_with_runloop: resolve_sym!(
                handle,
                b"IOHIDManagerScheduleWithRunLoop\0",
                FnIOHIDManagerScheduleWithRunLoop
            )?,
            manager_open: resolve_sym!(
                handle,
                b"IOHIDManagerOpen\0",
                FnIOHIDManagerOpen
            )?,
            manager_close: resolve_sym!(
                handle,
                b"IOHIDManagerClose\0",
                FnIOHIDManagerClose
            )?,
            device_open: resolve_sym!(
                handle,
                b"IOHIDDeviceOpen\0",
                FnIOHIDDeviceOpen
            )?,
            device_close: resolve_sym!(
                handle,
                b"IOHIDDeviceClose\0",
                FnIOHIDDeviceClose
            )?,
            device_get_location_id: resolve_sym!(
                handle,
                b"IOHIDDeviceGetLocationID\0",
                FnIOHIDDeviceGetLocationID
            )?,
            device_get_property: resolve_sym!(
                handle,
                b"IOHIDDeviceGetProperty\0",
                FnIOHIDDeviceGetProperty
            )?,
            device_create_queue: resolve_sym!(
                handle,
                b"IOHIDDeviceCreateQueue\0",
                FnIOHIDDeviceCreateQueue
            )?,
            queue_register_callback: resolve_sym!(
                handle,
                b"IOHIDQueueRegisterValueAvailableCallback\0",
                FnIOHIDQueueRegisterValueAvailableCallback
            )?,
            queue_schedule_with_runloop: resolve_sym!(
                handle,
                b"IOHIDQueueScheduleWithRunLoop\0",
                FnIOHIDQueueScheduleWithRunLoop
            )?,
            queue_open: resolve_sym!(
                handle,
                b"IOHIDQueueOpen\0",
                FnIOHIDQueueOpen
            )?,
            queue_close: resolve_sym!(
                handle,
                b"IOHIDQueueClose\0",
                FnIOHIDQueueClose
            )?,
            value_get_integer: resolve_sym!(
                handle,
                b"IOHIDValueGetInteger\0",
                FnIOHIDValueGetInteger
            )?,
            value_get_element: resolve_sym!(
                handle,
                b"IOHIDValueGetElement\0",
                FnIOHIDValueGetElement
            )?,
            element_get_usage_page: resolve_sym!(
                handle,
                b"IOHIDElementGetUsagePage\0",
                FnIOHIDElementGetUsagePage
            )?,
            element_get_usage: resolve_sym!(
                handle,
                b"IOHIDElementGetUsage\0",
                FnIOHIDElementGetUsage
            )?,
            cf_array_get_count: resolve_sym!(
                handle,
                b"CFArrayGetCount\0",
                FnCFArrayGetCount
            )?,
            cf_array_get_value_at_index: resolve_sym!(
                handle,
                b"CFArrayGetValueAtIndex\0",
                FnCFArrayGetValueAtIndex
            )?,
            cf_set_get_count: resolve_sym!(
                handle,
                b"CFSetGetCount\0",
                FnCFSetGetCount
            )?,
            cf_set_create_iterator: resolve_sym!(
                handle,
                b"CFSetCreateIterator\0",
                FnCFSetCreateIterator
            )?,
            cf_set_iterator_get_next: resolve_sym!(
                handle,
                b"CFSetIteratorGetNext\0",
                FnCFSetIteratorGetNext
            )?,
            cf_release: resolve_sym!(handle, b"CFRelease\0", FnCFRelease)?,
            device_copy_cf_type_arg_by_index: resolve_sym!(
                handle,
                b"IOHIDDeviceCopyCFTypeArgumentByIndex\0",
                FnIOHIDDeviceCopyCFTypeArgumentByIndex
            )?,
            cf_number_create: resolve_sym!(
                handle,
                b"CFNumberCreate\0",
                FnCFNumberCreate
            )?,
            cf_number_get_value: resolve_sym!(
                handle,
                b"CFNumberGetValue\0",
                FnCFNumberGetValue
            )?,
            cf_dict_create_mutable: resolve_sym!(
                handle,
                b"CFDictionaryCreateMutable\0",
                FnCFDictionaryCreateMutable
            )?,
            cf_dict_set_value: resolve_sym!(
                handle,
                b"CFDictionarySetValue\0",
                FnCFDictionarySetValue
            )?,
            manager_set_device_matching_with_dict: resolve_sym!(
                handle,
                b"IOHIDManagerSetDeviceMatchingWithDictionary\0",
                FnIOHIDManagerSetDeviceMatchingWithDictionary
            )?,
        };

        IOHID_FUNCS.set(funcs).map_err(|_| ())
    }

    /// Get the resolved function pointers.
    ///
    /// Panics if resolution failed. Callers must ensure `resolve()` succeeded
    /// before calling this.
    fn get() -> &'static Self {
        let _ = Self::resolve();
        IOHID_FUNCS
            .get()
            .expect("IOHID functions not resolved. Call resolve() first.")
    }
}

// Convenience wrappers that delegate to resolved function pointers.
// These provide the same API as the original extern "C" declarations.

unsafe fn IOHIDManagerCreate(
    allocator: CFAllocatorRef,
    options: u32,
) -> *mut IOHIDManager {
    unsafe { (IoKitFunctions::get().manager_create)(allocator, options) }
}

unsafe fn IOHIDManagerSetDeviceMatching(manager: *mut IOHIDManager) {
    unsafe { (IoKitFunctions::get().manager_set_device_matching)(manager) }
}

unsafe fn IOHIDManagerCopyDevices(manager: *mut IOHIDManager) -> CFSetRef {
    unsafe { (IoKitFunctions::get().manager_copy_devices)(manager) }
}

unsafe fn IOHIDManagerScheduleWithRunLoop(
    manager: *mut IOHIDManager,
    run_loop: *mut c_void,
    mode: *mut c_void,
) {
    unsafe {
        (IoKitFunctions::get().manager_schedule_with_runloop)(
            manager, run_loop, mode,
        )
    }
}

unsafe fn IOHIDManagerOpen(manager: *mut IOHIDManager, flags: u32) -> u32 {
    unsafe { (IoKitFunctions::get().manager_open)(manager, flags) }
}

unsafe fn IOHIDManagerClose(manager: *mut IOHIDManager, flags: u32) -> u32 {
    unsafe { (IoKitFunctions::get().manager_close)(manager, flags) }
}

unsafe fn IOHIDDeviceOpen(device: *mut IOHIDDevice, flags: u32) -> u32 {
    unsafe { (IoKitFunctions::get().device_open)(device, flags) }
}

unsafe fn IOHIDDeviceClose(device: *mut IOHIDDevice, flags: u32) -> u32 {
    unsafe { (IoKitFunctions::get().device_close)(device, flags) }
}

unsafe fn IOHIDDeviceGetLocationID(device: *mut IOHIDDevice) -> u32 {
    unsafe { (IoKitFunctions::get().device_get_location_id)(device) }
}

unsafe fn IOHIDDeviceGetProperty(
    device: *mut IOHIDDevice,
    property: CFStringRef,
) -> *const c_void {
    unsafe { (IoKitFunctions::get().device_get_property)(device, property) }
}

unsafe fn IOHIDDeviceCreateQueue(
    device: *mut IOHIDDevice,
    allocator: CFAllocatorRef,
) -> *mut IOHIDQueue {
    unsafe { (IoKitFunctions::get().device_create_queue)(device, allocator) }
}

unsafe fn IOHIDQueueRegisterValueAvailableCallback(
    queue: *mut IOHIDQueue,
    callout: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *mut IOHIDQueue,
            u32,
            *mut c_void, // CFArrayRef of IOHIDValueRef
        ),
    >,
    context: *mut c_void,
) {
    unsafe {
        (IoKitFunctions::get().queue_register_callback)(
            queue, callout, context,
        )
    }
}

unsafe fn IOHIDQueueScheduleWithRunLoop(
    queue: *mut IOHIDQueue,
    run_loop: *mut c_void,
    mode: *mut c_void,
) {
    unsafe {
        (IoKitFunctions::get().queue_schedule_with_runloop)(
            queue, run_loop, mode,
        )
    }
}

unsafe fn IOHIDQueueOpen(queue: *mut IOHIDQueue) -> u32 {
    unsafe { (IoKitFunctions::get().queue_open)(queue) }
}

unsafe fn IOHIDQueueClose(queue: *mut IOHIDQueue, flags: u32) {
    unsafe { (IoKitFunctions::get().queue_close)(queue, flags) }
}

unsafe fn IOHIDValueGetInteger(value: *mut IOHIDValue) -> u32 {
    unsafe { (IoKitFunctions::get().value_get_integer)(value) }
}

unsafe fn IOHIDValueGetElement(value: *mut IOHIDValue) -> *mut IOHIDElement {
    unsafe { (IoKitFunctions::get().value_get_element)(value) }
}

unsafe fn IOHIDElementGetUsagePage(element: *mut IOHIDElement) -> u32 {
    unsafe { (IoKitFunctions::get().element_get_usage_page)(element) }
}

unsafe fn IOHIDElementGetUsage(element: *mut IOHIDElement) -> u32 {
    unsafe { (IoKitFunctions::get().element_get_usage)(element) }
}

unsafe fn CFArrayGetCount(the_array: *const c_void) -> usize {
    unsafe { (IoKitFunctions::get().cf_array_get_count)(the_array) }
}

unsafe fn CFArrayGetValueAtIndex(
    the_array: *const c_void,
    idx: usize,
) -> *const c_void {
    unsafe {
        (IoKitFunctions::get().cf_array_get_value_at_index)(the_array, idx)
    }
}

unsafe fn CFSetGetCount(the_set: CFSetRef) -> usize {
    unsafe { (IoKitFunctions::get().cf_set_get_count)(the_set) }
}

unsafe fn CFSetCreateIterator(
    the_set: CFSetRef,
    iterator: *mut *mut c_void,
) -> bool {
    unsafe {
        (IoKitFunctions::get().cf_set_create_iterator)(the_set, iterator)
    }
}

unsafe fn CFSetIteratorGetNext(iterator: *mut c_void) -> CFTypeRef {
    unsafe { (IoKitFunctions::get().cf_set_iterator_get_next)(iterator) }
}

unsafe fn CFRelease(cf: *const c_void) {
    unsafe { (IoKitFunctions::get().cf_release)(cf) }
}

unsafe fn IOHIDDeviceCopyCFTypeArgumentByIndex(
    device: *mut IOHIDDevice,
    index: usize,
) -> *const c_void {
    unsafe {
        (IoKitFunctions::get().device_copy_cf_type_arg_by_index)(device, index)
    }
}

unsafe fn CFNumberCreate(
    allocator: CFAllocatorRef,
    the_type: u32,
    value_ptr: *const c_void,
) -> CFNumberRef {
    unsafe {
        (IoKitFunctions::get().cf_number_create)(
            allocator, the_type, value_ptr,
        )
    }
}

#[allow(improper_ctypes_definitions)]
unsafe fn CFNumberGetValue(
    number: CFNumberRef,
    the_type: u32,
    value_ptr: *mut c_void,
) -> bool {
    unsafe {
        (IoKitFunctions::get().cf_number_get_value)(
            number, the_type, value_ptr,
        )
    }
}

unsafe fn CFDictionaryCreateMutable(
    allocator: CFAllocatorRef,
    capacity: isize,
    key_type: *const c_void,
    value_type: *const c_void,
) -> *mut c_void {
    unsafe {
        (IoKitFunctions::get().cf_dict_create_mutable)(
            allocator, capacity, key_type, value_type,
        )
    }
}

unsafe fn CFDictionarySetValue(
    dict: *mut c_void,
    key: *const c_void,
    value: *const c_void,
) {
    unsafe { (IoKitFunctions::get().cf_dict_set_value)(dict, key, value) }
}

unsafe fn IOHIDManagerSetDeviceMatchingWithDictionary(
    manager: *mut IOHIDManager,
    dict: CFDictionaryRef,
) {
    unsafe {
        (IoKitFunctions::get().manager_set_device_matching_with_dict)(
            manager, dict,
        )
    }
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
    // Shared client to the Karabiner DriverKit virtual HID keyboard.
    pub conn: std::sync::Arc<super::karabiner_client::KarabinerClient>,
    // Bitmask tracking which modifier keys are physically pressed.
    pub modifier_state: u8,
    // Set of currently pressed keycodes for deduplication.
    pub pressed_keys: std::collections::HashSet<u16>,
    // Device location ID string for keyboard filtering.
    pub device_id: String,
    // Capture mode only: usage ids of non-modifier keys that were forwarded
    // (unmapped) and are still held.  The virtual keyboard report is a state
    // snapshot, so every forwarded report must include all of these.
    pub forwarded_keys: HashSet<u16>,
    // Capture mode only: full usage codes of keys whose key-down was mapped,
    // so their key-up is swallowed rather than forwarded.
    pub mapped_keys: HashSet<u32>,
    // Capture mode only: bitmask of modifier keys that were forwarded
    // (unmapped) and are still held.  Mapped modifiers are excluded so their
    // self-contained output taps do not leak into forwarded reports.
    pub forwarded_modifiers: u8,
}

// ---------------------------------------------------------------------------
// Capture mode
// ---------------------------------------------------------------------------
//
// Capture mode makes the daemon re-emit every key through the virtual
// keyboard (mapped keys as their mapped output, unmapped keys forwarded), so
// the e2e monitor can seize the virtual keyboard and capture the daemon's
// output without depending on a focused window.  It is gated on the
// `KEYMAPPER_CAPTURE` environment variable so production behaviour (unmapped
// keys dropped, key-ups ignored) is left untouched.

/// Set once from `start_mapping` to record whether capture mode is active.
static CAPTURE_MODE: AtomicBool = AtomicBool::new(false);

/// Whether capture mode is active (all keys re-emitted through the virtual
/// keyboard).
pub fn capture_enabled() -> bool {
    CAPTURE_MODE.load(Ordering::Relaxed)
}

/// Record the capture-mode flag determined at startup.
pub fn set_capture_mode(enabled: bool) {
    CAPTURE_MODE.store(enabled, Ordering::Relaxed);
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
    /// Open the device with the specified options.
    ///
    /// Use `kIOHIDOptionsTypeSeizeDevice` for exclusive access (Karabiner
    /// approach).  Requires root privileges.
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
        unsafe {
            IOHIDDeviceClose(self.device, kIOHIDOptionsTypeNone);
        }
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

    // Returns a string property of this device, or `None` if the property
    // is absent or not a string.
    fn string_property(&self, key: &str) -> Option<String> {
        let key_cf = create_cf_string(key);
        let value = unsafe { IOHIDDeviceGetProperty(self.device, key_cf) };
        unsafe { CFRelease(key_cf as *const _) };

        if value.is_null() {
            return None;
        }

        // The property is a CFString; the raw pointer can be used directly
        // as a `&CFString` reference (zero-sized CF type).
        let cf_string = unsafe { &*(value as *const CFString) };

        // UTF-8 needs at most 4 bytes per UTF-16 code unit (a surrogate
        // pair is two units encoding one 4-byte character).
        let capacity = (cf_string.length() as usize).saturating_mul(4) + 1;
        let mut buffer = vec![0u8; capacity];

        let ok = unsafe {
            cf_string.c_string(
                buffer.as_mut_ptr().cast::<std::ffi::c_char>(),
                capacity as CFIndex,
                CFStringBuiltInEncodings::EncodingUTF8.0,
            )
        };

        unsafe { CFRelease(value) };

        if !ok {
            return None;
        }

        // The buffer is NUL-terminated by CFStringGetCString.
        let len = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
        Some(String::from_utf8_lossy(&buffer[..len]).into_owned())
    }

    /// Returns true if this device is the Karabiner DriverKit virtual
    /// keyboard (our own output device).
    ///
    /// The virtual keyboard matches the generic keyboard matcher (usage
    /// page 0x01, usage 0x06), so it must be excluded from seizure to
    /// prevent a feedback loop.  Identified by the driver's hardcoded
    /// serial number, with a product-name prefix match as a secondary
    /// check.
    pub fn is_karabiner_virtual_keyboard(&self) -> bool {
        let serial = self.string_property(kIOHIDSerialNumberKey);
        let product = self.string_property(kIOHIDProductKey);
        matches_karabiner_virtual_keyboard(
            serial.as_deref(),
            product.as_deref(),
        )
    }

    // Returns the raw device reference.
    pub fn as_raw(&self) -> *mut IOHIDDevice {
        self.device
    }
}

/// Returns true if the given device properties identify the Karabiner
/// DriverKit virtual keyboard.
///
/// The serial number is the primary marker (exact match); the product name
/// is a secondary check (prefix match, because the suffix is the driver
/// version).
fn matches_karabiner_virtual_keyboard(
    serial: Option<&str>,
    product: Option<&str>,
) -> bool {
    serial.is_some_and(|s| s == KARABINER_VIRTUAL_KEYBOARD_SERIAL)
        || product.is_some_and(|p| {
            p.starts_with(KARABINER_VIRTUAL_KEYBOARD_PRODUCT_PREFIX)
        })
}

// ---------------------------------------------------------------------------
// HidQueue — receives HID values from a seized device
// ---------------------------------------------------------------------------

/// An `IOHIDQueue` that receives raw HID values from a seized device.
pub struct HidQueue {
    queue: *mut IOHIDQueue,
}

impl HidQueue {
    /// Register a callback for HID value events.
    ///
    /// The context will be leaked and freed when the queue is dropped.  This
    /// is safe because the queue outlives the context in normal operation, and
    /// we free it explicitly on drop.
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
/// each element.  Only key-down events are processed for remapping;
/// key-up and unmapped keys pass through naturally via the seized device.
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

        // Skip non-keyboard/consumer events.
        if usage_page != HID_USAGE_PAGE_KEYBOARD
            && usage_page != HID_USAGE_PAGE_CONSUMER
        {
            continue;
        }

        // Get the value (0 = up, non-zero = down).
        let raw_value = unsafe { IOHIDValueGetInteger(value_ref) };
        let is_down = raw_value != 0;

        // Construct HidUsage from raw HID page/id.  Use this for all
        // modifier tracking, deduplication, and key identification.
        let Some(hid_usage) =
            HidUsage::from_code(usage_page << 16 | (usage as u32))
        else {
            // Unknown usage — let it pass through.
            continue;
        };

        // Track pressed keys for deduplication.  Use the raw HID usage id
        // (page-specific, unambiguous).
        let key_id = hid_usage.id();

        if is_down {
            // Key-down.  Ignore auto-repeat (the key is already tracked).
            if !context.pressed_keys.insert(key_id) {
                continue;
            }

            // Get the device ID for keyboard filtering.
            let device_id = Some(context.device_id.as_str());

            // Track modifier state using HidUsage directly.  The lookup uses
            // the state captured before this key's own bit is set, so a bare
            // modifier trigger does not match itself.
            let lookup_modifiers = context.modifier_state;
            if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage) {
                context.modifier_state |= 1 << bit;
            }

            // Perform the lookup.  Compiled rules store the trigger as a
            // `HidUsage`, so the lookup is keyed by the full page-specific
            // usage.
            let guard = context.lookup.read();
            let active_outputs = guard
                .for_app(
                    &guard.active_app(),
                    hid_usage,
                    lookup_modifiers,
                    device_id,
                )
                .or_else(|| {
                    guard.global(hid_usage, lookup_modifiers, device_id)
                })
                .map(|v| v.to_vec());
            drop(guard);

            if let Some(outputs) = active_outputs {
                // Mapped: emit the mapped outputs via the virtual HID
                // keyboard.  In capture mode, remember the key was mapped so
                // its release is swallowed.
                for native_key in &outputs {
                    emit_hid_report(
                        &context.conn,
                        native_key,
                        &context.forwarded_keys,
                        context.forwarded_modifiers,
                    );
                }
                if capture_enabled() {
                    context.mapped_keys.insert(hid_usage.code());
                }
            } else if capture_enabled() {
                // Unmapped (capture mode): forward the key through the
                // virtual keyboard so the monitor can see it.
                forward_key_down(context, hid_usage);
            }
        } else if capture_enabled() {
            // Key-up (capture mode only).  Ignore releases for keys that were
            // never tracked as down.
            if !context.pressed_keys.remove(&key_id) {
                continue;
            }

            // Clear the modifier bit so subsequent forwarded reports carry the
            // correct modifier state.
            if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage) {
                context.modifier_state &= !(1 << bit);
            }

            // A mapped key's release is swallowed; a forwarded key's release
            // is forwarded.
            if !context.mapped_keys.remove(&hid_usage.code()) {
                forward_key_up(context, hid_usage);
            }
        } else {
            // Non-capture key-up: drop it (existing behaviour).
            context.pressed_keys.remove(&key_id);
        }
    }
}

/// Emit a single `NativeKey` through the Karabiner virtual keyboard.
///
/// Posts a report with the modifier and base key pressed, followed by a
/// release report.  Because the virtual keyboard report is a shared state
/// snapshot, both reports also carry the currently-held forwarded keys and
/// modifier byte so that emitting a mapped output does not clear them.
/// Dispatches on the output usage's page:
/// - Keyboard page (0x07): a 67-byte `keyboard_input` report (32 × 16-bit
///   usages).
/// - Consumer page (0x0C): a `consumer_input` report.
fn emit_hid_report(
    conn: &Arc<super::karabiner_client::KarabinerClient>,
    native_key: &crate::daemon::mapping_cache::NativeKey,
    forwarded_keys: &HashSet<u16>,
    forwarded_modifiers: u8,
) {
    use crate::common::hid_usage::PAGE_KEYBOARD;

    if native_key.usage.page() == PAGE_KEYBOARD {
        // Keyboard page: post the base usage together with every held
        // forwarded key, then a release report that keeps the forwarded keys
        // but drops the base.
        let mut usages: Vec<u16> = forwarded_keys.iter().copied().collect();
        usages.push(native_key.usage.id());
        usages.sort_unstable();

        let modifiers = native_key.modifiers | forwarded_modifiers;
        let _ = conn.send_keyboard_report(modifiers, &usages);

        // Release: the forwarded keys stay held; only the base is dropped.
        let release_usages: Vec<u16> =
            forwarded_keys.iter().copied().collect();
        let _ =
            conn.send_keyboard_report(forwarded_modifiers, &release_usages);
    } else {
        // Consumer page: post the usage, then an all-clear report to release.
        let _ = conn.send_consumer_report(native_key.usage.id());
        let _ = conn.send_consumer_release();
    }
}

/// Forward an unmapped key-down through the virtual keyboard (capture mode).
///
/// Keyboard-page keys are added to the held set and the full state snapshot
/// is posted; consumer-page keys are pressed directly.
fn forward_key_down(context: &mut HidQueueContext, hid_usage: HidUsage) {
    if hid_usage.page() == PAGE_CONSUMER {
        let _ = context.conn.send_consumer_report(hid_usage.id());
        return;
    }

    if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage) {
        // Modifier: tracked in the modifier byte, not a usage slot.
        context.forwarded_modifiers |= 1 << bit;
    } else {
        context.forwarded_keys.insert(hid_usage.id());
    }

    post_forwarded_state(
        &context.conn,
        &context.forwarded_keys,
        context.forwarded_modifiers,
    );
}

/// Forward an unmapped key-up through the virtual keyboard (capture mode).
fn forward_key_up(context: &mut HidQueueContext, hid_usage: HidUsage) {
    if hid_usage.page() == PAGE_CONSUMER {
        let _ = context.conn.send_consumer_release();
        return;
    }

    if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage) {
        context.forwarded_modifiers &= !(1 << bit);
    } else {
        context.forwarded_keys.remove(&hid_usage.id());
    }

    post_forwarded_state(
        &context.conn,
        &context.forwarded_keys,
        context.forwarded_modifiers,
    );
}

/// Post the current forwarded keyboard state as a `keyboard_input` report.
///
/// The report is a state snapshot: it carries every held forwarded key and the
/// forwarded modifier byte.  The virtual keyboard emits a down for each newly
/// present usage and an up for each usage that is no longer present.
fn post_forwarded_state(
    conn: &Arc<super::karabiner_client::KarabinerClient>,
    forwarded_keys: &HashSet<u16>,
    modifiers: u8,
) {
    let mut usages: Vec<u16> = forwarded_keys.iter().copied().collect();
    // Sort for a deterministic report layout (slot order is irrelevant to the
    // virtual keyboard's state tracking, but determinism aids debugging).
    usages.sort_unstable();
    let _ = conn.send_keyboard_report(modifiers, &usages);
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

                let hid_device = HidDevice {
                    device,
                    location_id,
                };

                // Skip the Karabiner DriverKit virtual keyboard: it matches
                // the generic keyboard matcher, and seizing our own output
                // device would create an infinite remap loop.
                if hid_device.is_karabiner_virtual_keyboard() {
                    println!(
                        "IOKit HID: skipping Karabiner virtual keyboard at \
                         location {}",
                        hid_device.location_id_string()
                    );
                    continue;
                }

                devices.push(hid_device);
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
/// Handle that keeps all seized devices and queues alive.  Drop to release.
pub struct SeizureHandle {
    _manager: HidDeviceManager,
    _devices: Vec<HidDevice>,
    _queue_handles: Vec<HidQueueHandle>,
}

// ---------------------------------------------------------------------------
// Entry point — IOKit device seizure with queue-based capture
// ---------------------------------------------------------------------------

/// Start keyboard input capture by seizing physical devices.
///
/// Discovers keyboards via `HidDeviceManager`, opens each with
/// `kIOHIDOptionsTypeSeizeDevice`, and creates an `IOHIDQueue` for event
/// delivery.  Mapped output is emitted through the shared `KarabinerClient`
/// (the Karabiner DriverKit virtual keyboard).
pub fn start_iohid_seizure_mapping(
    lookup: std::sync::Arc<
        parking_lot::RwLock<dyn crate::daemon::state::Lookup>,
    >,
    conn: std::sync::Arc<super::karabiner_client::KarabinerClient>,
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
            conn: conn.clone(),
            modifier_state: 0,
            pressed_keys: std::collections::HashSet::new(),
            device_id,
            forwarded_keys: HashSet::new(),
            mapped_keys: HashSet::new(),
            forwarded_modifiers: 0,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_bit_from_hid_usage_left_control() {
        let usage = HidUsage::LeftControl;
        assert_eq!(HidUsage::hid_usage_to_modifier_bit(usage), Some(0));
    }

    #[test]
    fn modifier_bit_from_hid_usage_non_modifier() {
        let usage = HidUsage::A;
        assert_eq!(HidUsage::hid_usage_to_modifier_bit(usage), None);
    }

    #[test]
    fn modifier_bit_from_hid_usage_consumer_page() {
        let usage = HidUsage::PlayPause;
        assert_eq!(HidUsage::hid_usage_to_modifier_bit(usage), None);
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

    // -----------------------------------------------------------------------
    // Karabiner virtual keyboard marker matching (feedback-loop filter)
    // -----------------------------------------------------------------------

    #[test]
    fn karabiner_marker_matches_by_serial_number() {
        assert!(matches_karabiner_virtual_keyboard(
            Some(KARABINER_VIRTUAL_KEYBOARD_SERIAL),
            None,
        ));
    }

    #[test]
    fn karabiner_marker_matches_by_product_prefix() {
        // The product string carries the driver version as a suffix.
        assert!(matches_karabiner_virtual_keyboard(
            None,
            Some("Karabiner DriverKit VirtualHIDKeyboard 1.8.0"),
        ));
    }

    #[test]
    fn karabiner_marker_matches_when_both_present() {
        assert!(matches_karabiner_virtual_keyboard(
            Some(KARABINER_VIRTUAL_KEYBOARD_SERIAL),
            Some("Karabiner DriverKit VirtualHIDKeyboard 1.8.0"),
        ));
    }

    #[test]
    fn karabiner_marker_rejects_physical_keyboard() {
        assert!(!matches_karabiner_virtual_keyboard(
            Some("ABC123"),
            Some("Magic Keyboard"),
        ));
    }

    #[test]
    fn karabiner_marker_rejects_missing_properties() {
        assert!(!matches_karabiner_virtual_keyboard(None, None));
    }

    #[test]
    fn karabiner_marker_rejects_similar_serial() {
        // A different serial must not match, even with a similar prefix.
        assert!(!matches_karabiner_virtual_keyboard(
            Some("pqrs.org:Karabiner-DriverKit-VirtualHIDKeyboard-clone"),
            None,
        ));
    }

    #[test]
    fn karabiner_marker_rejects_similar_product() {
        // The prefix match must not fire on unrelated product names that
        // merely share a word.
        assert!(!matches_karabiner_virtual_keyboard(
            None,
            Some("My Karabiner DriverKit VirtualHIDKeyboard Clone"),
        ));
    }
}
