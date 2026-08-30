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

use std::{collections::HashSet, ffi::c_void, ptr, sync::Arc};

use objc2_core_foundation::{
    CFIndex, CFRunLoop, CFString, CFStringBuiltInEncodings,
    kCFRunLoopDefaultMode,
};

use super::{
    INJECTION_KEYBOARD_IDENTITY, KeyboardIdentity, OUTPUT_KEYBOARD_IDENTITY,
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

/// `kCFNumberSInt32Type` — signed 32-bit integer CFNumber type.
///
/// The CF API has no unsigned 32-bit number type; signed 32-bit is the
/// correct choice for reading 16-bit VID/PID values. (An earlier revision
/// used the value `1` under a "UInt" name, but `1` is actually
/// `kCFNumberSInt8Type`, which silently truncated every numeric property
/// read to 8 bits.)
#[allow(non_upper_case_globals)]
const kCFNumberSInt32Type: u32 = 3;

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
    // The IOKit framework itself could not be loaded.
    FrameworkLoadFailed(String),
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
            IoKitError::FrameworkLoadFailed(ctx) => {
                write!(f, "failed to load the IOKit framework: {}", ctx)
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
type FnIOHIDManagerSetDeviceMatching =
    unsafe extern "C" fn(*mut IOHIDManager, CFDictionaryRef);
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
type FnIOHIDDeviceGetProperty =
    unsafe extern "C" fn(*mut IOHIDDevice, CFStringRef) -> *const c_void;
type FnIOHIDDeviceCopyMatchingElements = unsafe extern "C" fn(
    *mut IOHIDDevice,
    CFDictionaryRef,
    u32,
)
    -> *const c_void; // CFArrayRef of IOHIDElementRef
type FnIOHIDQueueCreate = unsafe extern "C" fn(
    CFAllocatorRef,
    *mut IOHIDDevice,
    CFIndex,
    u32,
) -> *mut IOHIDQueue;
type FnIOHIDQueueAddElement =
    unsafe extern "C" fn(*mut IOHIDQueue, *mut IOHIDElement);
type FnIOHIDQueueStart = unsafe extern "C" fn(*mut IOHIDQueue);
type FnIOHIDQueueStop = unsafe extern "C" fn(*mut IOHIDQueue);
type FnIOHIDQueueRegisterValueAvailableCallback = unsafe extern "C" fn(
    *mut IOHIDQueue,
    Option<
        unsafe extern "C" fn(
            *mut c_void,
            i32,             // IOReturn
            *mut IOHIDQueue, // sender: the queue itself
        ),
    >,
    *mut c_void,
);
type FnIOHIDQueueCopyNextValue =
    unsafe extern "C" fn(*mut IOHIDQueue) -> *mut IOHIDValue;
type FnIOHIDQueueScheduleWithRunLoop = unsafe extern "C" fn(
    *mut IOHIDQueue,
    *mut c_void, // CFRunLoopRef
    *mut c_void, // CFStringRef
);
type FnIOHIDValueGetIntegerValue =
    unsafe extern "C" fn(*mut IOHIDValue) -> CFIndex;
type FnIOHIDValueGetElement =
    unsafe extern "C" fn(*mut IOHIDValue) -> *mut IOHIDElement;
type FnIOHIDElementGetUsagePage =
    unsafe extern "C" fn(*mut IOHIDElement) -> u32;
type FnIOHIDElementGetUsage = unsafe extern "C" fn(*mut IOHIDElement) -> u32;
type FnCFArrayGetCount = unsafe extern "C" fn(*const c_void) -> usize;
type FnCFArrayGetValueAtIndex =
    unsafe extern "C" fn(*const c_void, usize) -> *const c_void;
type FnCFSetGetCount = unsafe extern "C" fn(CFSetRef) -> usize;
type FnCFSetApplyFunction = unsafe extern "C" fn(
    CFSetRef,
    unsafe extern "C" fn(*const c_void, *mut c_void),
    *mut c_void,
);
type FnCFRelease = unsafe extern "C" fn(*const c_void);
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
    device_get_property: FnIOHIDDeviceGetProperty,
    device_copy_matching_elements: FnIOHIDDeviceCopyMatchingElements,
    queue_create: FnIOHIDQueueCreate,
    queue_add_element: FnIOHIDQueueAddElement,
    queue_start: FnIOHIDQueueStart,
    queue_stop: FnIOHIDQueueStop,
    queue_register_callback: FnIOHIDQueueRegisterValueAvailableCallback,
    queue_schedule_with_runloop: FnIOHIDQueueScheduleWithRunLoop,
    queue_copy_next_value: FnIOHIDQueueCopyNextValue,
    value_get_integer_value: FnIOHIDValueGetIntegerValue,
    value_get_element: FnIOHIDValueGetElement,
    element_get_usage_page: FnIOHIDElementGetUsagePage,
    element_get_usage: FnIOHIDElementGetUsage,
    cf_array_get_count: FnCFArrayGetCount,
    cf_array_get_value_at_index: FnCFArrayGetValueAtIndex,
    cf_set_get_count: FnCFSetGetCount,
    cf_set_apply_function: FnCFSetApplyFunction,
    cf_release: FnCFRelease,
    cf_number_create: FnCFNumberCreate,
    cf_number_get_value: FnCFNumberGetValue,
    cf_dict_create_mutable: FnCFDictionaryCreateMutable,
    cf_dict_set_value: FnCFDictionarySetValue,
}

impl IoKitFunctions {
    /// Resolve all IOHIDLib symbols from IOKit at runtime.
    ///
    /// Returns `Ok(())` when all required symbols are available, or an
    /// [`IoKitError`] naming the missing symbol or the framework load
    /// failure.
    fn resolve() -> Result<(), IoKitError> {
        if IOHID_FUNCS.get().is_some() {
            return Ok(());
        }

        // Load the IOKit framework dynamically.
        let path = b"/System/Library/Frameworks/IOKit.framework/IOKit\0";
        let handle =
            unsafe { libc::dlopen(path.as_ptr() as *const _, libc::RTLD_NOW) };
        if handle.is_null() {
            let err_ptr = unsafe { libc::dlerror() };
            let msg = if err_ptr.is_null() {
                "unknown error".to_string()
            } else {
                unsafe { std::ffi::CStr::from_ptr(err_ptr) }
                    .to_string_lossy()
                    .into_owned()
            };
            return Err(IoKitError::FrameworkLoadFailed(msg));
        }

        // SAFETY: `Option<FnType>` uses niche optimization where null pointer
        // bits represent `None`.  Transmuting `*mut c_void` (from dlsym) to
        // `Option<FnType>` is valid because both have identical size and
        // alignment, and the null/non-null bit patterns match.
        macro_rules! resolve_sym {
            ($handle:expr, $name:expr, $ty:ty) => {{
                let c_name = std::str::from_utf8($name)
                    .expect("symbol names are valid UTF-8");
                let raw = unsafe {
                    libc::dlsym($handle, c_name.as_ptr() as *const _)
                };
                let opt: Option<$ty> = unsafe { std::mem::transmute(raw) };
                // Strip the trailing NUL so the error names the symbol
                // cleanly.
                let name = c_name.strip_suffix('\0').unwrap_or(c_name);
                opt.ok_or_else(|| IoKitError::SymbolMissing(name.to_string()))
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
            device_get_property: resolve_sym!(
                handle,
                b"IOHIDDeviceGetProperty\0",
                FnIOHIDDeviceGetProperty
            )?,
            device_copy_matching_elements: resolve_sym!(
                handle,
                b"IOHIDDeviceCopyMatchingElements\0",
                FnIOHIDDeviceCopyMatchingElements
            )?,
            queue_create: resolve_sym!(
                handle,
                b"IOHIDQueueCreate\0",
                FnIOHIDQueueCreate
            )?,
            queue_add_element: resolve_sym!(
                handle,
                b"IOHIDQueueAddElement\0",
                FnIOHIDQueueAddElement
            )?,
            queue_start: resolve_sym!(
                handle,
                b"IOHIDQueueStart\0",
                FnIOHIDQueueStart
            )?,
            queue_stop: resolve_sym!(
                handle,
                b"IOHIDQueueStop\0",
                FnIOHIDQueueStop
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
            queue_copy_next_value: resolve_sym!(
                handle,
                b"IOHIDQueueCopyNextValue\0",
                FnIOHIDQueueCopyNextValue
            )?,
            value_get_integer_value: resolve_sym!(
                handle,
                b"IOHIDValueGetIntegerValue\0",
                FnIOHIDValueGetIntegerValue
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
            cf_set_apply_function: resolve_sym!(
                handle,
                b"CFSetApplyFunction\0",
                FnCFSetApplyFunction
            )?,
            cf_release: resolve_sym!(handle, b"CFRelease\0", FnCFRelease)?,
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
        };

        // A concurrent `resolve()` may have won the race and stored its own
        // copy; that is fine, so ignore the `set` failure.
        let _ = IOHID_FUNCS.set(funcs);
        Ok(())
    }

    /// Get the resolved function pointers.
    ///
    /// Resolves on first use and panics with a descriptive error if the IOKit
    /// framework or any required symbol is unavailable.
    fn get() -> &'static Self {
        Self::resolve().unwrap_or_else(|e| {
            panic!("failed to resolve IOHID functions: {e}")
        });
        IOHID_FUNCS
            .get()
            .expect("IOHID functions not stored after successful resolve")
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

unsafe fn IOHIDManagerSetDeviceMatching(
    manager: *mut IOHIDManager,
    matching: CFDictionaryRef,
) {
    unsafe {
        (IoKitFunctions::get().manager_set_device_matching)(manager, matching)
    }
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

unsafe fn IOHIDDeviceGetProperty(
    device: *mut IOHIDDevice,
    property: CFStringRef,
) -> *const c_void {
    unsafe { (IoKitFunctions::get().device_get_property)(device, property) }
}

unsafe fn IOHIDDeviceCopyMatchingElements(
    device: *mut IOHIDDevice,
    matching: CFDictionaryRef,
    options: u32,
) -> *const c_void {
    unsafe {
        (IoKitFunctions::get().device_copy_matching_elements)(
            device, matching, options,
        )
    }
}

unsafe fn IOHIDQueueCreate(
    allocator: CFAllocatorRef,
    device: *mut IOHIDDevice,
    depth: CFIndex,
    options: u32,
) -> *mut IOHIDQueue {
    unsafe {
        (IoKitFunctions::get().queue_create)(allocator, device, depth, options)
    }
}

unsafe fn IOHIDQueueAddElement(
    queue: *mut IOHIDQueue,
    element: *mut IOHIDElement,
) {
    unsafe { (IoKitFunctions::get().queue_add_element)(queue, element) }
}

unsafe fn IOHIDQueueRegisterValueAvailableCallback(
    queue: *mut IOHIDQueue,
    callout: Option<
        unsafe extern "C" fn(
            *mut c_void,
            i32,             // IOReturn
            *mut IOHIDQueue, // sender: the queue itself
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

unsafe fn IOHIDQueueCopyNextValue(
    queue: *mut IOHIDQueue,
) -> Option<*mut IOHIDValue> {
    let value =
        unsafe { (IoKitFunctions::get().queue_copy_next_value)(queue) };
    (!value.is_null()).then_some(value)
}

unsafe fn IOHIDQueueStart(queue: *mut IOHIDQueue) {
    unsafe { (IoKitFunctions::get().queue_start)(queue) }
}

unsafe fn IOHIDQueueStop(queue: *mut IOHIDQueue) {
    unsafe { (IoKitFunctions::get().queue_stop)(queue) }
}

unsafe fn IOHIDValueGetIntegerValue(value: *mut IOHIDValue) -> CFIndex {
    unsafe { (IoKitFunctions::get().value_get_integer_value)(value) }
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

unsafe fn CFSetApplyFunction(
    the_set: CFSetRef,
    applier: unsafe extern "C" fn(*const c_void, *mut c_void),
    context: *mut c_void,
) {
    unsafe {
        (IoKitFunctions::get().cf_set_apply_function)(
            the_set, applier, context,
        )
    }
}

unsafe fn CFRelease(cf: *const c_void) {
    unsafe { (IoKitFunctions::get().cf_release)(cf) }
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
    // Usage ids of non-modifier keys that were forwarded (unmapped) and are
    // still held.  The virtual keyboard report is a state snapshot, so every
    // forwarded report must include all of these.
    pub forwarded_keys: HashSet<u16>,
    // Full usage codes of keys whose key-down was mapped, so their key-up is
    // swallowed rather than forwarded.
    pub mapped_keys: HashSet<u32>,
    // Bitmask of modifier keys that were forwarded (unmapped) and are still
    // held.  Mapped modifiers are excluded so their self-contained output
    // taps do not leak into forwarded reports.
    pub forwarded_modifiers: u8,
    // Bitmask of modifier keys that were part of a fired trigger and have
    // already been released on the virtual keyboard.  Their physical release
    // is swallowed so it is not forwarded a second time.
    pub consumed_modifiers: u8,
}

// ---------------------------------------------------------------------------
// Event forwarding
// ---------------------------------------------------------------------------
//
// Every key is re-emitted through the virtual keyboard: mapped keys as their
// mapped output, unmapped keys forwarded unchanged.  A seized keyboard is
// invisible to the OS, so anything not re-emitted would be lost; forwarding
// unmapped keys keeps the seized keyboards usable for normal typing.

// ---------------------------------------------------------------------------
// HidDevice — represents a discovered HID keyboard device
// ---------------------------------------------------------------------------

/// A single HID keyboard device discovered via IOHIDManager.
///
/// Holds the `IOHIDDeviceRef` for filtering and event capture.
pub struct HidDevice {
    // Raw device reference.
    device: *mut IOHIDDevice,
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
                "IOHIDDeviceOpen for device at location {}",
                self.location_id_string(),
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
            IOHIDQueueCreate(
                kCFAllocatorDefault,
                self.device,
                1024,
                kIOHIDOptionsTypeNone,
            )
        };

        if queue.is_null() {
            return Err(IoKitError::IoReturn(
                0xfffffff0, // kIOReturnBadArgument
                "IOHIDQueueCreate returned null".into(),
            ));
        }

        // Register every element of the device so the queue receives all
        // input events (a null matching dictionary returns all elements).
        let elements = unsafe {
            IOHIDDeviceCopyMatchingElements(
                self.device,
                ptr::null(),
                kIOHIDOptionsTypeNone,
            )
        };

        if !elements.is_null() {
            let count = unsafe { CFArrayGetCount(elements) };
            for i in 0..count {
                let element = unsafe { CFArrayGetValueAtIndex(elements, i) }
                    as *mut IOHIDElement;
                if !element.is_null() {
                    unsafe { IOHIDQueueAddElement(queue, element) };
                }
            }
            unsafe { CFRelease(elements) };
        }

        Ok(HidQueue { queue })
    }

    // Returns a stable identifier for this device as a hex string.
    //
    // The modern IOHID API no longer exposes a location ID, so the device
    // pointer itself is used; it is unique for the lifetime of the device.
    pub fn location_id_string(&self) -> String {
        format!("0x{:x}", self.device as usize)
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

        // The value is a borrowed reference (the "Get" function does not
        // transfer ownership), so it must not be released; IOKit caches the
        // CFString and reuses it across devices.

        if !ok {
            return None;
        }

        // The buffer is NUL-terminated by CFStringGetCString.
        let len = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
        Some(String::from_utf8_lossy(&buffer[..len]).into_owned())
    }

    // Returns a numeric (CFNumber) property of this device as `u32`, or
    // `None` if the property is absent or not a number.
    fn number_property(&self, key: &str) -> Option<u32> {
        let key_cf = create_cf_string(key);
        let value = unsafe { IOHIDDeviceGetProperty(self.device, key_cf) };
        unsafe { CFRelease(key_cf as *const _) };

        if value.is_null() {
            return None;
        }

        let mut num: u32 = 0;
        // `CFNumberGetValue` converts from the stored CFNumber type, so
        // requesting an unsigned 32-bit value works for any integer type.
        let ok = unsafe {
            CFNumberGetValue(
                value as CFNumberRef,
                kCFNumberSInt32Type,
                &mut num as *mut u32 as *mut c_void,
            )
        };

        // The value is a borrowed reference (the "Get" function does not
        // transfer ownership), so it must not be released.
        if !ok {
            return None;
        }
        Some(num)
    }

    /// Returns the device's (vendor ID, product ID) pair, or `None` if
    /// either property is absent.
    ///
    /// The pqrs DriverKit driver registers the no-space keys `VendorID` /
    /// `ProductID`, while on macOS 26 the standard `kIOHIDMapKeyVendorID` /
    /// `kIOHIDMapKeyProductID` keys ("Vendor ID" / "Product ID") return
    /// null for every device.  Each key is tried in turn, so the lookup
    /// works for both real and virtual keyboards across OS versions.
    pub fn vendor_product_id(&self) -> Option<(u32, u32)> {
        let vendor_id = self
            .number_property(kIOHIDMapKeyVendorID)
            .or_else(|| self.number_property("VendorID"))?;
        let product_id = self
            .number_property(kIOHIDMapKeyProductID)
            .or_else(|| self.number_property("ProductID"))?;
        Some((vendor_id, product_id))
    }

    /// Returns true if this device is the daemon's output keyboard.
    ///
    /// The output keyboard matches the generic keyboard matcher, so it must
    /// be excluded from seizure to prevent a feedback loop.  Identified by
    /// its VID/PID, which the driver takes from the initialize request.
    pub fn is_output_keyboard(&self) -> bool {
        identity_matches(self.vendor_product_id(), OUTPUT_KEYBOARD_IDENTITY)
    }

    /// Returns true if this device is the e2e injection keyboard.
    ///
    /// The test harness creates a second virtual keyboard with this
    /// identity; the daemon seizes it like any other physical keyboard so
    /// injected keys flow through the normal remap path.
    pub fn is_injection_keyboard(&self) -> bool {
        identity_matches(self.vendor_product_id(), INJECTION_KEYBOARD_IDENTITY)
    }

    /// Build a platform-agnostic [`KeyboardInfo`] from this device's IOKit
    /// properties, using the same field derivation as the `ioreg`-based
    /// enumeration so that keyboard filters match consistently.
    pub fn keyboard_info(&self) -> crate::common::keyboard::KeyboardInfo {
        use crate::common::keyboard::KeyboardInfo;

        let name = self
            .string_property(kIOHIDProductKey)
            .unwrap_or_else(|| "<unknown>".to_string());

        let vendor_id = self.number_property(kIOHIDMapKeyVendorID);
        let product_id = self.number_property(kIOHIDMapKeyProductID);

        let vendor = vendor_id
            .map(super::keyboard::vendor_id_to_name)
            .unwrap_or_else(|| "<unknown>".to_string());

        let model = match (vendor_id, product_id) {
            (Some(vid), Some(pid)) => format!("0x{:04x}:0x{:04x}", vid, pid),
            (Some(vid), None) => format!("0x{:04x}", vid),
            _ => "<unknown>".to_string(),
        };

        let port = self.string_property("Transport");

        KeyboardInfo::new(name, vendor, model, self.location_id_string(), port)
    }

    // Returns the raw device reference.
    pub fn as_raw(&self) -> *mut IOHIDDevice {
        self.device
    }
}

/// Returns true if the given (vendor ID, product ID) pair matches the
/// keyboard identity.
fn identity_matches(
    vid_pid: Option<(u32, u32)>,
    identity: KeyboardIdentity,
) -> bool {
    vid_pid.is_some_and(|(vid, pid)| {
        vid == identity.vendor_id as u32 && pid == identity.product_id as u32
    })
}

// ---------------------------------------------------------------------------
// HidQueue — receives HID values from a seized device
// ---------------------------------------------------------------------------

/// An `IOHIDQueue` that receives raw HID values from a seized device.
pub struct HidQueue {
    queue: *mut IOHIDQueue,
}

/// Signature of an `IOHIDQueue` value-available callback.
///
/// Matches the C `IOHIDCallback` type: `(void *context, IOReturn result,
/// void *sender)`, where `sender` is the queue itself.  The values are not
/// passed in; the callback must drain the queue via `IOHIDQueueCopyNextValue`
/// (non-blocking) until it returns NULL.
pub type HidValueCallback = unsafe extern "C" fn(
    user_info: *mut c_void,
    result: i32,
    queue: *mut IOHIDQueue,
);

impl HidQueue {
    /// Register the daemon's remapping callback with a `HidQueueContext`.
    ///
    /// The context is boxed and passed to the callback as `user_info`; it is
    /// freed when the returned handle is dropped.
    pub fn register_value_callback(
        &self,
        context: HidQueueContext,
    ) -> HidQueueHandle<HidQueueContext> {
        self.register_value_callback_generic(hid_queue_value_callback, context)
    }

    /// Register an arbitrary value-available callback with a caller-provided
    /// context.
    ///
    /// The context is boxed and passed to the callback as `user_info`; it is
    /// freed when the returned handle is dropped.  This lets callers (e.g. the
    /// e2e monitor) register their own logging callback instead of the
    /// daemon's remapping one.
    pub fn register_value_callback_generic<T>(
        &self,
        callback: HidValueCallback,
        context: T,
    ) -> HidQueueHandle<T> {
        let context_ptr = Box::into_raw(Box::new(context));

        unsafe {
            IOHIDQueueRegisterValueAvailableCallback(
                self.queue,
                Some(callback),
                context_ptr as *mut c_void,
            );
        }

        HidQueueHandle {
            queue: self.queue,
            context_ptr,
        }
    }

    // Schedule the queue with the current run loop.
    pub fn schedule_with_runloop(&self) {
        let run_loop =
            CFRunLoop::current().expect("IOHIDQueue: no current run loop");
        let mode_ref = unsafe { kCFRunLoopDefaultMode }
            .expect("kCFRunLoopDefaultMode is always available");

        // `CFRetained` is a smart pointer; the FFI call needs the underlying
        // CF object pointer, not the address of the wrapper.  Passing the
        // wrapper's address makes `CFRunLoopAddSource` fail its PAC check.
        unsafe {
            IOHIDQueueScheduleWithRunLoop(
                self.queue,
                &*run_loop as *const _ as *mut c_void,
                mode_ref as *const _ as *mut c_void,
            );
        }
    }

    // Open (start) the queue.  Must be called after registering callbacks.
    pub fn open(&self) -> Result<(), IoKitError> {
        // `IOHIDQueueStart` is infallible in the modern API (void return).
        unsafe { IOHIDQueueStart(self.queue) };
        Ok(())
    }
}

/// FFI callback invoked by IOHIDQueue when values are available.
///
/// Matches the C `IOHIDCallback` signature: `(void *context, IOReturn
/// result, void *sender)`, where `sender` is the queue.  The values are not
/// passed in; they must be drained from the queue with
/// `IOHIDQueueCopyNextValue` (non-blocking) until it returns NULL.  Each
/// returned value is a retained copy and must be released after processing.
unsafe extern "C" fn hid_queue_value_callback(
    user_info: *mut c_void,
    _result: i32,
    queue: *mut IOHIDQueue,
) {
    if user_info.is_null() || queue.is_null() {
        return;
    }

    let context = unsafe { &mut *(user_info as *mut HidQueueContext) };

    // Drain the queue; each value is a retained copy that must be released.
    while let Some(value_ref) = unsafe { IOHIDQueueCopyNextValue(queue) } {
        process_hid_value(context, value_ref);
        unsafe { CFRelease(value_ref as *const _) };
    }
}

/// Process a single `IOHIDValue` from the seized device's queue.
///
/// Extracts usage page, usage code, and value from the element that produced
/// the value, then dispatches to [`process_key_event`].  Only keyboard and
/// consumer page values are processed; all other values are ignored.
fn process_hid_value(
    context: &mut HidQueueContext,
    value_ref: *mut IOHIDValue,
) {
    // Get the element that produced this value.
    let element = unsafe { IOHIDValueGetElement(value_ref) };
    if element.is_null() {
        return;
    }

    // Extract usage page and usage code.
    let usage_page = unsafe { IOHIDElementGetUsagePage(element) };
    let usage = unsafe { IOHIDElementGetUsage(element) } as u16;

    // Skip non-keyboard/consumer events.
    if usage_page != HID_USAGE_PAGE_KEYBOARD
        && usage_page != HID_USAGE_PAGE_CONSUMER
    {
        return;
    }

    // Get the value (0 = up, non-zero = down).
    let raw_value = unsafe { IOHIDValueGetIntegerValue(value_ref) };
    let is_down = raw_value != 0;

    // Construct HidUsage from raw HID page/id.  Use this for all
    // modifier tracking, deduplication, and key identification.
    let Some(hid_usage) =
        HidUsage::from_code(usage_page << 16 | (usage as u32))
    else {
        // Unknown usage — let it pass through.
        return;
    };

    process_key_event(context, hid_usage, is_down);
}

/// Process a single key event (down or up) identified by its `HidUsage`.
///
/// This is the core of the capture logic, invoked by the IOKit queue
/// callback for every seized keyboard (physical keyboards and the e2e
/// injection keyboard alike).  It performs deduplication, modifier tracking,
/// rule lookup, and emission/forwarding.
fn process_key_event(
    context: &mut HidQueueContext,
    hid_usage: HidUsage,
    is_down: bool,
) {
    // Track pressed keys for deduplication.  Use the raw HID usage id
    // (page-specific, unambiguous).
    let key_id = hid_usage.id();

    if is_down {
        // Key-down.  Ignore auto-repeat (the key is already tracked).
        if !context.pressed_keys.insert(key_id) {
            return;
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
            .or_else(|| guard.global(hid_usage, lookup_modifiers, device_id))
            .map(|v| v.to_vec());
        drop(guard);

        if let Some(outputs) = active_outputs {
            // The trigger's modifiers were forwarded when pressed.  Release
            // them now so the output is emitted as a clean tap: holding them
            // would produce an unintended control sequence (e.g. the rule
            // Ctrl+Semicolon -> C would emit Ctrl+C, i.e. SIGINT).  Mark them
            // consumed so their physical release is swallowed below.
            let consumed = lookup_modifiers & context.forwarded_modifiers;
            if consumed != 0 {
                context.forwarded_modifiers &= !consumed;
                context.consumed_modifiers |= consumed;
                post_forwarded_state(
                    &context.conn,
                    &context.forwarded_keys,
                    context.forwarded_modifiers,
                );
            }

            // Mapped: emit the mapped outputs via the virtual HID keyboard.
            // Remember the key was mapped so its release is swallowed rather
            // than forwarded.
            for native_key in &outputs {
                emit_hid_report(
                    &context.conn,
                    native_key,
                    &context.forwarded_keys,
                    context.forwarded_modifiers,
                );
            }
            context.mapped_keys.insert(hid_usage.code());
        } else {
            // Unmapped: forward the key through the virtual keyboard so it
            // reaches the OS unchanged.
            forward_key_down(context, hid_usage);
        }
    } else {
        // Key-up.  Ignore releases for keys that were never tracked as down.
        if !context.pressed_keys.remove(&key_id) {
            return;
        }

        // Clear the modifier bit so subsequent forwarded reports carry the
        // correct modifier state.
        if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage) {
            context.modifier_state &= !(1 << bit);
        }

        // A consumed modifier (part of a fired trigger) was already released
        // on the virtual keyboard when the trigger fired; swallow its release.
        if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage)
            && context.consumed_modifiers & (1 << bit) != 0
        {
            context.consumed_modifiers &= !(1 << bit);
            return;
        }

        // A mapped key's release is swallowed; a forwarded key's release
        // is forwarded.
        if !context.mapped_keys.remove(&hid_usage.code()) {
            forward_key_up(context, hid_usage);
        }
    }
}

/// Drain a queue of `IOHIDValueRef` and invoke `f` for each recognized
/// keyboard/consumer key event, passing the combined HID usage code
/// (`(page << 16) | id`) and whether the key is down.
///
/// Values are pulled from the queue with `IOHIDQueueCopyNextValue`
/// (non-blocking) until it returns NULL; each returned value is a retained
/// copy and is released after processing.  Non-keyboard/consumer values are
/// skipped.  Used by the e2e monitor's logging callback to extract key
/// events from the seized virtual keyboard.
///
/// # Safety
///
/// `queue` must be a valid, open `IOHIDQueue` that outlives the call, and
/// the callback `f` must not panic (a panic would leak the retained value
/// currently being processed).
pub unsafe fn for_each_hid_value(
    queue: *mut IOHIDQueue,
    mut f: impl FnMut(u32, bool),
) {
    if queue.is_null() {
        return;
    }

    while let Some(value_ref) = unsafe { IOHIDQueueCopyNextValue(queue) } {
        let element = unsafe { IOHIDValueGetElement(value_ref) };

        if !element.is_null() {
            let usage_page = unsafe { IOHIDElementGetUsagePage(element) };

            if usage_page == HID_USAGE_PAGE_KEYBOARD
                || usage_page == HID_USAGE_PAGE_CONSUMER
            {
                let usage = unsafe { IOHIDElementGetUsage(element) };
                let raw_value =
                    unsafe { IOHIDValueGetIntegerValue(value_ref) };

                f((usage_page << 16) | usage, raw_value != 0);
            }
        }

        // Each value is a retained copy; release it after processing.
        unsafe { CFRelease(value_ref as *const _) };
    }
}

/// Emit a single `NativeKey` through the Karabiner virtual keyboard.
///
/// Emits the output as a sequence of state-snapshot reports so that every key
/// transition is its own report: each output modifier down (ascending bit
/// order), the base key down, the base key up, then each output modifier up
/// (descending bit order).  Posting one transition per report makes the
/// captured event order deterministic — a single report carrying both a
/// modifier and the base key would let IOKit deliver the two values in an
/// unspecified order.  Because each report is a full state snapshot, it also
/// carries the currently-held forwarded keys and modifier byte so that
/// emitting a mapped output does not clear them (already-held keys are
/// repeats the monitor suppresses).
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
        let base_usage = native_key.usage.id();
        let output_modifiers = native_key.modifiers;

        // State snapshots with and without the base key.  Sorted for a
        // deterministic report layout (slot order is irrelevant to the
        // virtual keyboard's state tracking, but determinism aids debugging).
        let mut usages_with_base: Vec<u16> =
            forwarded_keys.iter().copied().collect();
        usages_with_base.push(base_usage);
        usages_with_base.sort_unstable();

        let mut usages_without_base: Vec<u16> =
            forwarded_keys.iter().copied().collect();
        usages_without_base.sort_unstable();

        // Press each output modifier, one at a time in ascending bit order,
        // so the captured event order is deterministic.
        let mut modifiers = forwarded_modifiers;
        for bit in 0..8 {
            if (output_modifiers >> bit) & 1 == 1 {
                modifiers |= 1 << bit;
                let _ =
                    conn.send_keyboard_report(modifiers, &usages_without_base);
            }
        }

        // Press the base key with all output modifiers held, then release it.
        let _ = conn.send_keyboard_report(modifiers, &usages_with_base);
        let _ = conn.send_keyboard_report(modifiers, &usages_without_base);

        // Release each output modifier, one at a time in descending bit order.
        for bit in (0..8).rev() {
            if (output_modifiers >> bit) & 1 == 1 {
                modifiers &= !(1 << bit);
                let _ =
                    conn.send_keyboard_report(modifiers, &usages_without_base);
            }
        }
    } else {
        // Consumer page: post the usage, then an all-clear report to release.
        let _ = conn.send_consumer_report(native_key.usage.id());
        let _ = conn.send_consumer_release();
    }
}

/// Forward an unmapped key-down through the virtual keyboard.
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

/// Forward an unmapped key-up through the virtual keyboard.
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
///
/// Generic over the context type `T` so callers can register their own
/// callback with their own context (see
/// [`HidQueue::register_value_callback_generic`]).  The context is freed on
/// drop.
pub struct HidQueueHandle<T> {
    queue: *mut IOHIDQueue,
    context_ptr: *mut T,
}

impl<T> Drop for HidQueueHandle<T> {
    fn drop(&mut self) {
        unsafe {
            IOHIDQueueStop(self.queue);
            if !self.context_ptr.is_null() {
                drop(Box::from_raw(self.context_ptr));
            }
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
                kCFNumberSInt32Type,
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
                kCFNumberSInt32Type,
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
            IOHIDManagerSetDeviceMatching(manager, dict);
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

        // Collect the matched devices.  The applier is a raw C function
        // pointer, so the accumulator travels in the context argument.
        unsafe {
            CFSetApplyFunction(
                device_set,
                scan_devices_applier,
                &mut devices as *mut Vec<HidDevice> as *mut c_void,
            );
        }

        // Release the CFSet.
        unsafe { CFRelease(device_set as *const _) };

        devices
    }

    /// Return the number of devices currently matched by this manager,
    /// including both Karabiner DriverKit virtual keyboards (unlike
    /// [`Self::scan_devices`], which skips the output keyboard).
    ///
    /// Used by the e2e monitor to log progress while waiting for the
    /// virtual keyboard to appear.
    pub fn matched_device_count(&self) -> usize {
        let device_set = unsafe { IOHIDManagerCopyDevices(self.manager) };

        if device_set.is_null() {
            return 0;
        }

        let count = unsafe { CFSetGetCount(device_set) };

        // Release the CFSet.
        unsafe { CFRelease(device_set as *const _) };

        count
    }

    /// Find the daemon's output keyboard among the matched devices, if it
    /// is currently connected.
    ///
    /// Matches on the output keyboard's VID/PID, so the e2e injection
    /// keyboard (a distinct identity) is never returned.  Unlike
    /// [`Self::scan_devices`], which skips the output keyboard (to prevent
    /// a remap feedback loop), this returns it — the e2e monitor seizes it
    /// to capture the daemon's output.
    pub fn find_karabiner_virtual_keyboard(&self) -> Option<HidDevice> {
        let device_set = unsafe { IOHIDManagerCopyDevices(self.manager) };

        if device_set.is_null() {
            return None;
        }

        let mut result = None;

        // `CFSetApplyFunction` has no early exit, so the applier stops
        // recording once a match has been found.
        unsafe {
            CFSetApplyFunction(
                device_set,
                find_karabiner_applier,
                &mut result as *mut Option<HidDevice> as *mut c_void,
            );
        }

        // Release the CFSet.
        unsafe { CFRelease(device_set as *const _) };

        result
    }

    /// Returns true if any Karabiner DriverKit virtual keyboard is currently
    /// connected — either the daemon's output keyboard or the e2e injection
    /// keyboard.
    ///
    /// Unlike [`Self::scan_devices`] (which skips the output keyboard and
    /// logs) and [`Self::find_karabiner_virtual_keyboard`] (which matches
    /// only the output keyboard), this matches both identities and does not
    /// log, so it is safe to call in a poll loop.  The e2e harness uses it
    /// to wait for stale virtual keyboard nodes to be destroyed before
    /// starting the monitor and daemon.
    pub fn has_karabiner_virtual_keyboard(&self) -> bool {
        let device_set = unsafe { IOHIDManagerCopyDevices(self.manager) };

        if device_set.is_null() {
            return false;
        }

        let mut found = false;

        // `CFSetApplyFunction` has no early exit, so the applier stops
        // checking once a match has been found.
        unsafe {
            CFSetApplyFunction(
                device_set,
                karabiner_virtual_keyboard_applier,
                &mut found as *mut bool as *mut c_void,
            );
        }

        // Release the CFSet.
        unsafe { CFRelease(device_set as *const _) };

        found
    }

    // Schedule the manager with the current run loop for hotplug support.
    pub fn schedule_with_runloop(&self) {
        let run_loop = CFRunLoop::current()
            .expect("HidDeviceManager: no current run loop");
        let mode_ref = unsafe { kCFRunLoopDefaultMode }
            .expect("kCFRunLoopDefaultMode is always available");

        // `CFRetained` is a smart pointer; the FFI call needs the underlying
        // CF object pointer, not the address of the wrapper.  Passing the
        // wrapper's address makes `CFRunLoopAddSource` fail its PAC check.
        unsafe {
            IOHIDManagerScheduleWithRunLoop(
                self.manager,
                &*run_loop as *const _ as *mut c_void,
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

/// `CFSetApplyFunction` applier for [`HidDeviceManager::scan_devices`].
///
/// Collects every matched device into the `Vec<HidDevice>` passed as the
/// context, skipping only the daemon's output keyboard: it matches the
/// generic keyboard matcher, and seizing our own output device would
/// create an infinite remap loop.  The e2e injection keyboard is included
/// on purpose — the daemon seizes it like any other physical keyboard.
unsafe extern "C" fn scan_devices_applier(
    value: *const c_void,
    info: *mut c_void,
) {
    let devices = unsafe { &mut *(info as *mut Vec<HidDevice>) };
    let device = value as *mut IOHIDDevice;

    let hid_device = HidDevice { device };

    if hid_device.is_output_keyboard() {
        println!(
            "IOKit HID: skipping the output keyboard at location {}",
            hid_device.location_id_string()
        );
        return;
    }

    devices.push(hid_device);
}

/// `CFSetApplyFunction` applier for
/// [`HidDeviceManager::find_karabiner_virtual_keyboard`].
///
/// Records the first device that identifies as the daemon's output keyboard
/// in the `Option<HidDevice>` passed as the context.  The e2e injection
/// keyboard has a distinct identity and is never matched.
unsafe extern "C" fn find_karabiner_applier(
    value: *const c_void,
    info: *mut c_void,
) {
    let result = unsafe { &mut *(info as *mut Option<HidDevice>) };

    // A match was already found; nothing left to do.
    if result.is_some() {
        return;
    }

    let device = value as *mut IOHIDDevice;
    let hid_device = HidDevice { device };

    if hid_device.is_output_keyboard() {
        *result = Some(hid_device);
    }
}

/// `CFSetApplyFunction` applier for
/// [`HidDeviceManager::has_karabiner_virtual_keyboard`].
///
/// Sets the `bool` passed as the context once any device identifies as a
/// Karabiner DriverKit virtual keyboard (output or injection identity).
unsafe extern "C" fn karabiner_virtual_keyboard_applier(
    value: *const c_void,
    info: *mut c_void,
) {
    let found = unsafe { &mut *(info as *mut bool) };

    // A match was already found; nothing left to do.
    if *found {
        return;
    }

    let device = value as *mut IOHIDDevice;
    let hid_device = HidDevice { device };

    if hid_device.is_output_keyboard() || hid_device.is_injection_keyboard() {
        *found = true;
    }
}

// ---------------------------------------------------------------------------
// Helper: create a CFString from &str (leaked, caller must CFRelease)
// ---------------------------------------------------------------------------

/// Create a `CFString` from a Rust string slice.  The returned pointer
/// must be released with `CFRelease` when no longer needed.
fn create_cf_string(s: &str) -> CFStringRef {
    use objc2::rc::Retained;
    use objc2_foundation::NSString;

    let ns = NSString::from_str(s);
    // CFString and NSString are toll-free bridged.  Consume the `Retained`
    // so its +1 retain count is transferred to the raw pointer; the caller
    // owns it and must release it with `CFRelease`.  (Extracting a pointer
    // from a borrowed reference and letting the `Retained` drop would free
    // the object and leave the returned pointer dangling.)
    let ptr: *mut objc2_foundation::NSString = Retained::into_raw(ns);
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
    _queue_handles: Vec<HidQueueHandle<HidQueueContext>>,
}

// ---------------------------------------------------------------------------
// Entry point — IOKit device seizure with queue-based capture
// ---------------------------------------------------------------------------

/// Whether a device passes the global keyboard filter.
///
/// Returns `true` when the filter is unset or empty (all keyboards pass), or
/// when the device matches at least one specifier.
fn device_matches_filter(
    device: &HidDevice,
    filter: Option<&[crate::common::keyboard::KeyboardSpecifier]>,
) -> bool {
    let Some(specs) = filter else {
        return true;
    };
    if specs.is_empty() {
        return true;
    }
    let info = device.keyboard_info();
    specs.iter().any(|spec| spec.matches(&info))
}

/// Start keyboard input capture by seizing physical devices.
///
/// Discovers keyboards via `HidDeviceManager`, opens each with
/// `kIOHIDOptionsTypeSeizeDevice`, and creates an `IOHIDQueue` for event
/// delivery.  Every key is re-emitted through the shared `KarabinerClient`
/// (the Karabiner DriverKit virtual keyboard): mapped keys as their mapped
/// output, unmapped keys forwarded unchanged.
pub fn start_iohid_seizure_mapping(
    lookup: std::sync::Arc<
        parking_lot::RwLock<dyn crate::daemon::state::Lookup>,
    >,
    conn: std::sync::Arc<super::karabiner_client::KarabinerClient>,
    keyboard_filter: Option<&[crate::common::keyboard::KeyboardSpecifier]>,
) -> Result<SeizureHandle, IoKitError> {
    // Discover physical keyboards.
    let manager = HidDeviceManager::new_keyboard_matcher()?;
    let discovered = manager.scan_devices();

    if discovered.is_empty() {
        return Err(IoKitError::IoReturn(
            0,
            "No keyboard devices found via IOHIDManager".into(),
        ));
    }

    println!(
        "IOKit HID: discovered {} keyboard device(s)",
        discovered.len(),
    );

    // Apply the global keyboard filter: only seize keyboards the user wants
    // to remap.  Non-matching keyboards are left alone so they keep working
    // normally (a seized keyboard is invisible to the OS).
    let devices: Vec<_> = discovered
        .into_iter()
        .filter(|device| device_matches_filter(device, keyboard_filter))
        .collect();

    if devices.is_empty() {
        eprintln!(
            "IOKit HID: no keyboards match the global filter; nothing to \
             seize"
        );
    }

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
            consumed_modifiers: 0,
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
    // Keyboard identity matching (feedback-loop filter)
    // -----------------------------------------------------------------------

    #[test]
    fn identity_matches_output_keyboard() {
        assert!(identity_matches(
            Some((0x16c0, 0x27db)),
            OUTPUT_KEYBOARD_IDENTITY
        ));
    }

    #[test]
    fn identity_matches_injection_keyboard() {
        assert!(identity_matches(
            Some((0x16c0, 0x27dc)),
            INJECTION_KEYBOARD_IDENTITY
        ));
    }

    #[test]
    fn identity_rejects_cross_match() {
        // The output keyboard must not match the injection identity and
        // vice versa; that distinction is what breaks the feedback loop.
        assert!(!identity_matches(
            Some((0x16c0, 0x27db)),
            INJECTION_KEYBOARD_IDENTITY
        ));
        assert!(!identity_matches(
            Some((0x16c0, 0x27dc)),
            OUTPUT_KEYBOARD_IDENTITY
        ));
    }

    #[test]
    fn identity_rejects_physical_keyboard() {
        assert!(!identity_matches(
            Some((0x046d, 0x0037)),
            OUTPUT_KEYBOARD_IDENTITY
        ));
        assert!(!identity_matches(
            Some((0x046d, 0x0037)),
            INJECTION_KEYBOARD_IDENTITY
        ));
    }

    #[test]
    fn identity_rejects_missing_properties() {
        assert!(!identity_matches(None, OUTPUT_KEYBOARD_IDENTITY));
        assert!(!identity_matches(None, INJECTION_KEYBOARD_IDENTITY));
    }
}

// ---------------------------------------------------------------------------
// Throwaway PoC: two concurrent Karabiner virtual keyboards
// ---------------------------------------------------------------------------
//
// Verification gate for the second-keyboard injector design (see
// `.sketches/Injector revised.md`).  This module proves or refutes two
// assumptions about the upstream pqrs driver:
//
// 1. The daemon hosts two concurrent client connections, each creating its own
//    virtual keyboard device node.
// 2. Each device's IOKit `Vendor ID` / `Product ID` properties reflect the
//    identity it was initialized with, while both share the driver's hardcoded
//    serial number (so VID/PID, not the serial, is the usable discriminator).
//
// The module is throwaway: delete it once the gate has been evaluated.
// It is skipped unless `KEYMAPPER_POC_TWO_KEYBOARDS=1` is set.  Running it
// requires root (the daemon socket lives in a root-only directory) and a
// live Karabiner DriverKit extension.  Stop `keymapperd` first so its
// output keyboard does not muddy the enumeration:
//
// ```sh
// sudo -E PATH="$PATH" env KEYMAPPER_POC_TWO_KEYBOARDS=1 \
//     $(which cargo) nextest run --no-capture poc_two_concurrent_keyboards
// ```

#[cfg(test)]
mod poc {
    use std::{
        io::{ErrorKind, Read, Write},
        os::unix::net::UnixStream,
        process::Command,
        sync::atomic::{AtomicBool, Ordering},
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    /// Path of the Karabiner daemon's UNIX stream socket (mirrors
    /// `karabiner_client::SOCKET_PATH`).
    const SOCKET_PATH: &str = "/Library/Application \
                               Support/org.pqrs/tmp/rootonly/\
                               karabiner_virtual_hid_device_service.sock";

    // Minimal protocol constants (mirrors `karabiner_client`).
    const PROTOCOL_VERSION: u16 = 7;
    const MSG_HEARTBEAT: u8 = 0;
    const MSG_REQUEST: u8 = 4;
    const MSG_RESPONSE: u8 = 5;
    const REQ_KEYBOARD_INITIALIZE: u8 = 0;
    const RESP_VIRTUAL_HID_KEYBOARD_READY: u8 = 4;

    /// A decoded frame from the daemon.
    struct PocFrame {
        msg_type: u8,
        body: Vec<u8>,
    }

    impl PocFrame {
        /// The 8-byte big-endian request ID, if the body carries one.
        fn request_id(&self) -> Option<u64> {
            self.body
                .get(..8)
                .map(|b| u64::from_be_bytes(b.try_into().unwrap()))
        }

        /// The payload after the request ID (a sequence of state pairs).
        fn payload(&self) -> &[u8] {
            self.body.get(8..).unwrap_or(&[])
        }

        /// Whether the frame reports the virtual keyboard as ready.
        fn is_ready(&self) -> bool {
            matches!(self.msg_type, MSG_REQUEST | MSG_RESPONSE)
                && self.payload().as_chunks::<2>().0.iter().any(|pair| {
                    pair[0] == RESP_VIRTUAL_HID_KEYBOARD_READY && pair[1] == 1
                })
        }
    }

    /// Encode a frame: `[4-byte BE u32 body_size][1-byte msg_type][body]`,
    /// where `body_size = 1 + len(body)`.
    fn encode_frame(msg_type: u8, body: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(5 + body.len());
        frame.extend_from_slice(&(1u32 + body.len() as u32).to_be_bytes());
        frame.push(msg_type);
        frame.extend_from_slice(body);
        frame
    }

    /// Encode a request/response frame whose body is an 8-byte big-endian
    /// request ID followed by a payload.
    fn encode_id_frame(
        msg_type: u8,
        request_id: u64,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut body = Vec::with_capacity(8 + payload.len());
        body.extend_from_slice(&request_id.to_be_bytes());
        body.extend_from_slice(payload);
        encode_frame(msg_type, &body)
    }

    /// A frame reader that tolerates read timeouts mid-frame.
    ///
    /// `read_exact` on a stream with a read timeout can return
    /// `WouldBlock` after consuming part of a frame; the partial bytes
    /// must be retained for the next call, so they are buffered here.
    struct FrameReader {
        stream: UnixStream,
        pending: Vec<u8>,
    }

    impl FrameReader {
        fn new(stream: UnixStream) -> Self {
            Self {
                stream,
                pending: Vec::new(),
            }
        }

        fn set_read_timeout(
            &self,
            timeout: Option<Duration>,
        ) -> std::io::Result<()> {
            self.stream.set_read_timeout(timeout)
        }

        fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.stream.write_all(bytes)
        }

        /// Read one complete frame, or `None` if no data arrived within
        /// the read timeout.
        fn read_frame(&mut self) -> std::io::Result<Option<PocFrame>> {
            loop {
                // A complete frame needs at least 5 bytes (4 length + 1
                // type).
                if self.pending.len() >= 5 {
                    let body_size = u32::from_be_bytes(
                        self.pending[0..4].try_into().unwrap(),
                    ) as usize;
                    if !(1..=65_536).contains(&body_size) {
                        return Err(std::io::Error::new(
                            ErrorKind::InvalidData,
                            format!("invalid frame body size {body_size}"),
                        ));
                    }
                    if self.pending.len() >= 4 + body_size {
                        let frame = PocFrame {
                            msg_type: self.pending[4],
                            body: self.pending[5..4 + body_size].to_vec(),
                        };
                        self.pending.drain(..4 + body_size);
                        return Ok(Some(frame));
                    }
                }

                let mut buf = [0u8; 4096];
                match self.stream.read(&mut buf) {
                    Ok(0) => {
                        return Err(std::io::Error::new(
                            ErrorKind::ConnectionAborted,
                            "connection closed",
                        ));
                    }
                    Ok(n) => {
                        self.pending.extend_from_slice(&buf[..n]);
                        continue;
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        return Ok(None);
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }

    /// Build the 24-byte `virtual_hid_keyboard_parameters` payload:
    /// `[vendor_id: u64 LE][product_id: u64 LE][country_code: u64 LE]`.
    fn keyboard_parameters(vendor_id: u64, product_id: u64) -> [u8; 24] {
        let mut params = [0u8; 24];
        params[0..8].copy_from_slice(&vendor_id.to_le_bytes());
        params[8..16].copy_from_slice(&product_id.to_le_bytes());
        // Country code `not_supported` (zero) is left as-is.
        params
    }

    /// Build the `REQ_KEYBOARD_INITIALIZE` request frame for the given
    /// identity.
    fn initialize_frame(vendor_id: u64, product_id: u64) -> Vec<u8> {
        let mut payload = Vec::with_capacity(3 + 24);
        payload.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        payload.push(REQ_KEYBOARD_INITIALIZE);
        payload.extend_from_slice(&keyboard_parameters(vendor_id, product_id));
        encode_id_frame(MSG_REQUEST, 1, &payload)
    }

    /// Build an empty response frame for the given request ID.
    fn response_frame(request_id: u64) -> Vec<u8> {
        encode_id_frame(MSG_RESPONSE, request_id, &[])
    }

    /// A virtual keyboard held open by a single daemon connection.
    ///
    /// Dropping it closes the connection, which destroys the device node.
    struct PocKeyboard {
        shutdown: Arc<AtomicBool>,
        reader: Option<thread::JoinHandle<()>>,
    }

    impl PocKeyboard {
        /// Connect to the daemon and initialize a virtual keyboard with the
        /// given identity, blocking until the daemon reports it as ready.
        fn new(vendor_id: u64, product_id: u64) -> Result<Self, String> {
            let stream = UnixStream::connect(SOCKET_PATH).map_err(|e| {
                format!("cannot connect to the Karabiner daemon: {e}")
            })?;
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .map_err(|e| e.to_string())?;

            let mut frames = FrameReader::new(stream);

            frames
                .write_all(&initialize_frame(vendor_id, product_id))
                .map_err(|e| e.to_string())?;

            // Read frames until the keyboard is ready, answering each state
            // update (the daemon stalls its reporting otherwise).
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                match frames.read_frame() {
                    Ok(Some(frame)) => {
                        // Answer before checking readiness: the ready frame
                        // itself may be a state update that must be answered.
                        if frame.msg_type == MSG_REQUEST
                            && let Some(id) = frame.request_id()
                        {
                            frames
                                .write_all(&response_frame(id))
                                .map_err(|e| e.to_string())?;
                        }
                        if frame.is_ready() {
                            break;
                        }
                    }
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            return Err("timed out waiting for the virtual \
                                        keyboard to become ready"
                                .into());
                        }
                    }
                    Err(e) => return Err(format!("socket read failed: {e}")),
                }
            }

            // Hand the connection to a keepalive thread that answers state
            // updates and sends heartbeats for as long as the keyboard is
            // held.
            let shutdown = Arc::new(AtomicBool::new(false));
            let reader_shutdown = Arc::clone(&shutdown);
            let reader = thread::Builder::new()
                .name("poc-karabiner-keepalive".into())
                .spawn(move || keepalive(frames, &reader_shutdown))
                .map_err(|e| e.to_string())?;

            Ok(Self {
                shutdown,
                reader: Some(reader),
            })
        }
    }

    impl Drop for PocKeyboard {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Release);
            if let Some(handle) = self.reader.take() {
                let _ = handle.join();
            }
        }
    }

    /// Keep a connection alive: answer state updates and send heartbeats.
    ///
    /// The daemon destroys the virtual keyboard when the connection closes,
    /// so this must outlive any read timeout: `Ok(None)` (no data within
    /// the timeout) is the normal outcome and must not end the loop.
    fn keepalive(mut frames: FrameReader, shutdown: &AtomicBool) {
        // A short timeout keeps the loop responsive to shutdown.
        let _ = frames.set_read_timeout(Some(Duration::from_millis(200)));

        let mut last_heartbeat = Instant::now();
        loop {
            if shutdown.load(Ordering::Acquire) {
                return;
            }

            let now = Instant::now();
            if now.duration_since(last_heartbeat) >= Duration::from_secs(3) {
                if frames.write_all(&encode_frame(MSG_HEARTBEAT, &[])).is_err()
                {
                    return;
                }
                last_heartbeat = now;
            }

            match frames.read_frame() {
                Ok(Some(frame)) => {
                    // Answer state updates; responses need no action.
                    if frame.msg_type == MSG_REQUEST
                        && let Some(id) = frame.request_id()
                        && frames.write_all(&response_frame(id)).is_err()
                    {
                        return;
                    }
                }
                // No data within the read timeout is normal; keep waiting.
                Ok(None) => {}
                // The connection is gone; nothing left to do.
                Err(_) => return,
            }
        }
    }

    /// A keyboard device as seen by the PoC. Vendor and product IDs are
    /// read via both the standard HID keys ("Vendor ID", "Product ID") and
    /// the driver's no-space registry keys ("VendorID", "ProductID"), since
    /// the standard keys return null on this OS.
    struct PocDevice {
        name: String,
        serial: Option<String>,
        vid_std: Option<u32>,
        pid_std: Option<u32>,
        vid_nospace: Option<u32>,
        pid_nospace: Option<u32>,
    }

    /// Enumerate every keyboard device matched by the given manager,
    /// including the Karabiner virtual keyboards (unlike `scan_devices`,
    /// which skips them).
    fn enumerate_all(manager: &HidDeviceManager) -> Vec<PocDevice> {
        let device_set = unsafe { IOHIDManagerCopyDevices(manager.manager) };
        if device_set.is_null() {
            return Vec::new();
        }

        let mut devices = Vec::new();
        unsafe {
            CFSetApplyFunction(
                device_set,
                collect_applier,
                &mut devices as *mut Vec<PocDevice> as *mut c_void,
            );
        }
        unsafe { CFRelease(device_set as *const _) };

        devices
    }

    /// Print every matched device with its identity properties.
    fn print_devices(devices: &[PocDevice]) {
        println!(
            "poc: {} keyboard device(s) visible to IOHIDManager:",
            devices.len()
        );
        for d in devices {
            println!(
                "  - product={:?} serial={:?} std_vid={:?} std_pid={:?} \
                 nospace_vid={:?} nospace_pid={:?}",
                d.name,
                d.serial,
                d.vid_std,
                d.pid_std,
                d.vid_nospace,
                d.pid_nospace
            );
        }
    }

    /// Run `ioreg` with the given arguments and return its stdout.
    fn run_ioreg(args: &[&str]) -> String {
        let Ok(output) = Command::new("ioreg").args(args).output() else {
            return "poc: failed to run ioreg".into();
        };
        if !output.status.success() {
            return format!("poc: ioreg exited with {}", output.status);
        }
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Keep only the lines of a full registry dump that mention the
    /// Karabiner driver (case-insensitive), with enough context around each
    /// match to include the node's header and property dictionary.
    fn filter_karabiner_lines(dump: &str) -> String {
        const CONTEXT: usize = 60;
        let lines: Vec<&str> = dump.lines().collect();
        let mut keep = vec![false; lines.len()];
        for (i, line) in lines.iter().enumerate() {
            let lower = line.to_ascii_lowercase();
            if lower.contains("karabiner") || lower.contains("virtualhid") {
                let start = i.saturating_sub(CONTEXT);
                let end = (i + CONTEXT).min(lines.len().saturating_sub(1));
                for kept in &mut keep[start..=end] {
                    *kept = true;
                }
            }
        }

        let mut out = String::new();
        for (line, &kept) in lines.iter().zip(&keep) {
            if kept {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }

    /// Capture the IORegistry state while the connections are still alive.
    /// `IOHIDDeviceGetProperty` does not return VID/PID/serial on this OS,
    /// so the registry is the source of truth. The targeted
    /// `AppleUserHIDDevice` class query returns nothing here, so also take
    /// a full dump and keep only the Karabiner-related lines (with context)
    /// to find the nodes' actual class and properties. The result is only
    /// written to the diagnostics file — a full dump overflows the console.
    fn capture_ioreg() -> String {
        let targeted = run_ioreg(&[
            "-p",
            "IOService",
            "-r",
            "-c",
            "AppleUserHIDDevice",
            "-l",
            "-w",
            "0",
        ]);
        let full = run_ioreg(&["-l", "-w", "0"]);
        let filtered = filter_karabiner_lines(&full);

        format!(
            "=== ioreg -c AppleUserHIDDevice (targeted) \
             ===\n{targeted}\n\n=== ioreg full dump, karabiner/virtualhid \
             lines with context ===\n{filtered}"
        )
    }

    /// Write the full diagnostic detail to a file so it can be inspected
    /// without overflowing the console: the baseline and live device lists,
    /// plus the registry dump.
    fn write_diagnostics(
        baseline: &[PocDevice],
        devices: &[PocDevice],
        ioreg: &str,
    ) {
        let path = "/tmp/keymapper_poc_diag.txt";
        let Ok(mut file) = std::fs::File::create(path) else {
            eprintln!("poc: cannot write diagnostics to {path}");
            return;
        };

        let _ = writeln!(
            file,
            "=== baseline devices (no virtual keyboards): {} ===",
            baseline.len()
        );
        for d in baseline {
            let _ = writeln!(
                file,
                "  product={:?} vid_std={:?} pid_std={:?} nospace_vid={:?} \
                 nospace_pid={:?} serial={:?}",
                d.name,
                d.vid_std,
                d.pid_std,
                d.vid_nospace,
                d.pid_nospace,
                d.serial
            );
        }
        let _ = writeln!(
            file,
            "\n=== devices with virtual keyboards alive: {} ===",
            devices.len()
        );
        for d in devices {
            let _ = writeln!(
                file,
                "  product={:?} vid_std={:?} pid_std={:?} nospace_vid={:?} \
                 nospace_pid={:?} serial={:?}",
                d.name,
                d.vid_std,
                d.pid_std,
                d.vid_nospace,
                d.pid_nospace,
                d.serial
            );
        }
        let _ = writeln!(file, "\n=== ioreg capture ===\n{ioreg}");

        eprintln!("poc: diagnostics written to {path}");
    }

    /// `CFSetApplyFunction` applier that records every matched device.
    unsafe extern "C" fn collect_applier(
        value: *const c_void,
        info: *mut c_void,
    ) {
        let devices = unsafe { &mut *(info as *mut Vec<PocDevice>) };
        let device = value as *mut IOHIDDevice;
        let hid = HidDevice { device };
        devices.push(PocDevice {
            name: hid.string_property(kIOHIDProductKey).unwrap_or_default(),
            serial: hid.string_property(kIOHIDSerialNumberKey),
            vid_std: hid.number_property(kIOHIDMapKeyVendorID),
            pid_std: hid.number_property(kIOHIDMapKeyProductID),
            vid_nospace: hid.number_property("VendorID"),
            pid_nospace: hid.number_property("ProductID"),
        });
    }

    #[test]
    fn poc_two_concurrent_keyboards() {
        if std::env::var("KEYMAPPER_POC_TWO_KEYBOARDS").is_err() {
            eprintln!(
                "poc: skipped (set KEYMAPPER_POC_TWO_KEYBOARDS=1 to run)"
            );
            return;
        }

        const OUTPUT_ID: (u32, u32) = (0x16c0, 0x27db);
        const INJECTION_ID: (u32, u32) = (0x16c0, 0x27dc);
        // The driver's hardcoded serial; both keyboards share it, which is
        // why the production code matches on VID/PID instead.
        const SHARED_SERIAL: &str =
            "pqrs.org:Karabiner-DriverKit-VirtualHIDKeyboard";

        println!("poc: initializing keyboard A with {OUTPUT_ID:?}");
        let kb_a = PocKeyboard::new(OUTPUT_ID.0 as u64, OUTPUT_ID.1 as u64)
            .expect("keyboard A (output identity) failed to initialize");

        println!("poc: initializing keyboard B with {INJECTION_ID:?}");
        let kb_b =
            PocKeyboard::new(INJECTION_ID.0 as u64, INJECTION_ID.1 as u64)
                .expect(
                    "keyboard B (injection identity) failed to initialize",
                );

        // Create the manager once and schedule it with the current run
        // loop. `IOHIDManagerCopyDevices` only reflects devices the manager
        // has been notified about, and those notifications are delivered
        // through the run loop (the same reason the monitor pumps while
        // waiting for the virtual keyboard).
        let manager = HidDeviceManager::new_keyboard_matcher()
            .expect("failed to create the IOHIDManager");
        manager.schedule_with_runloop();

        // Let the run loop register the existing (non-virtual) devices so
        // we have a stable baseline to compare against.
        for _ in 0..20 {
            CFRunLoop::run_in_mode(
                unsafe { kCFRunLoopDefaultMode },
                0.1,
                true,
            );
        }
        let baseline = enumerate_all(&manager);

        // Poll until the deadline, pumping the run loop so hotplug
        // notifications are processed.
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut last_log = Instant::now();
        let mut devices = enumerate_all(&manager);
        loop {
            if Instant::now() >= deadline {
                break;
            }

            // Pump the run loop so hotplug notifications are processed; a
            // short timeout keeps the deadline check responsive.
            CFRunLoop::run_in_mode(
                unsafe { kCFRunLoopDefaultMode },
                0.1,
                true,
            );

            // Log progress once per second so a stuck wait is diagnosable.
            if last_log.elapsed() >= Duration::from_secs(1) {
                last_log = Instant::now();
                eprintln!(
                    "poc: waiting; {} device(s) matched (baseline {})",
                    devices.len(),
                    baseline.len()
                );
            }

            devices = enumerate_all(&manager);
        }

        print_devices(&devices);

        // The standard HID keys ("Vendor ID", "Product ID") return null on
        // this OS; the registry stores the driver's no-space keys instead.
        // Report which devices expose them through IOHIDDeviceGetProperty —
        // if the two virtual keyboards do, Phase 2 can read VID/PID with no
        // registry correlation.
        let exposed: Vec<&PocDevice> = devices
            .iter()
            .filter(|d| d.vid_nospace.is_some() || d.pid_nospace.is_some())
            .collect();
        println!(
            "poc: no-space key probe — {} device(s) exposed VendorID/ \
             ProductID via GetProperty:",
            exposed.len()
        );
        for d in &exposed {
            println!(
                "  - product={:?} vendor_id={:?} product_id={:?}",
                d.name, d.vid_nospace, d.pid_nospace
            );
        }

        // Capture the registry state while the connections are still alive,
        // and write the full detail to a file (the dump is too large for
        // the console).
        let ioreg = capture_ioreg();
        write_diagnostics(&baseline, &devices, &ioreg);

        let delta = devices.len().saturating_sub(baseline.len());
        println!(
            "poc: baseline {} device(s) -> {} with keyboards alive (delta \
             +{})",
            baseline.len(),
            devices.len(),
            delta
        );

        // The capture is too large for the console; it is only in the
        // diagnostics file.
        let ioreg_lines = ioreg.lines().count();
        println!(
            "poc: ioreg capture has {ioreg_lines} lines; see \
             /tmp/keymapper_poc_diag.txt"
        );

        // The gate: two distinct virtual keyboards, each with its own
        // VID/PID, both sharing the driver's hardcoded serial.
        // `IOHIDDeviceGetProperty` does not return these under the standard
        // HID keys on this OS, so verify against the registry, where the
        // driver stores them under the no-space key "ProductID" (decimal or
        // hex PID forms). Checking only "ProductID" lines keeps unrelated
        // hex addresses from causing a false positive.
        let pid_lines: Vec<&str> = ioreg
            .lines()
            .filter(|line| line.contains("ProductID"))
            .collect();
        let has_output = pid_lines.iter().any(|line| {
            line.contains(&OUTPUT_ID.1.to_string())
                || line.contains(&format!("{:x}", OUTPUT_ID.1))
        });
        let has_injection = pid_lines.iter().any(|line| {
            line.contains(&INJECTION_ID.1.to_string())
                || line.contains(&format!("{:x}", INJECTION_ID.1))
        });
        let serial_count = ioreg.matches(SHARED_SERIAL).count();

        println!(
            "poc: registry check — {} \"ProductID\" line(s), output pid \
             present: {has_output}, injection pid present: {has_injection}, \
             serial occurrences: {serial_count}",
            pid_lines.len()
        );

        if has_output && has_injection && serial_count >= 2 {
            println!(
                "poc: PASS — two concurrent virtual keyboards with distinct \
                 VID/PIDs and a shared serial"
            );
        } else {
            println!(
                "poc: FAIL — expected both PIDs and a shared serial in the \
                 registry; see /tmp/keymapper_poc_diag.txt"
            );
        }

        // Drop the keyboards (closes the connections, destroys the nodes).
        drop(kb_b);
        drop(kb_a);
    }
}
