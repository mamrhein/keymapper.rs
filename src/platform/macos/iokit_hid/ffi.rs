// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Raw IOKit/CF FFI bindings for the IOHIDLib API.
//!
//! On modern macOS with SIP, the IOKit framework is a "stub" — the actual
//! IOHIDLib symbols are only accessible at runtime via dlopen/dlsym, not at
//! link time.  All symbols are therefore resolved dynamically (see
//! [`IoKitFunctions`]) and exposed through the convenience wrappers at the
//! bottom of this module, which provide the same API as plain `extern "C"`
//! declarations would.

use std::{ffi::c_void, ptr};

use objc2_core_foundation::CFIndex;

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
pub(super) type CFSetRef = *mut c_void;

/// Opaque `CFAllocatorRef` (we use null for `kCFAllocatorDefault`).
#[allow(non_camel_case_types)]
pub(super) type CFAllocatorRef = *mut c_void;

/// Opaque `CFDictionaryRef`.
#[allow(non_camel_case_types)]
pub(super) type CFDictionaryRef = *const c_void;

/// Opaque `CFNumberRef`.
#[allow(non_camel_case_types)]
pub(super) type CFNumberRef = *const c_void;

/// Opaque `CFStringRef`.
#[allow(non_camel_case_types)]
pub(super) type CFStringRef = *const c_void;

/// `kCFAllocatorDefault` is represented as NULL.
#[allow(non_upper_case_globals)]
pub(super) const kCFAllocatorDefault: CFAllocatorRef = ptr::null_mut();

/// `kCFNumberSInt32Type` — signed 32-bit integer CFNumber type.
///
/// The CF API has no unsigned 32-bit number type; signed 32-bit is the
/// correct choice for reading 16-bit VID/PID values. (An earlier revision
/// used the value `1` under a "UInt" name, but `1` is actually
/// `kCFNumberSInt8Type`, which silently truncated every numeric property
/// read to 8 bits.)
#[allow(non_upper_case_globals)]
pub(super) const kCFNumberSInt32Type: u32 = 3;

/// `kIOHIDOptionsTypeNone`.
#[allow(non_upper_case_globals)]
pub(super) const kIOHIDOptionsTypeNone: u32 = 0;

/// `kIOHIDOptionsTypeSeizeDevice` — exclusive access to the device.
#[allow(non_upper_case_globals)]
pub(super) const kIOHIDOptionsTypeSeizeDevice: u32 = 1;

/// `kIOHIDMapKeyLocationID`.
#[allow(non_upper_case_globals)]
pub(super) const kIOHIDMapKeyLocationID: &str = "Location ID";

/// `kIOHIDMapKeyVendorID`.
#[allow(non_upper_case_globals)]
pub(super) const kIOHIDMapKeyVendorID: &str = "Vendor ID";

/// `kIOHIDMapKeyProductID`.
#[allow(non_upper_case_globals)]
pub(super) const kIOHIDMapKeyProductID: &str = "Product ID";

/// `kIOHIDMapKeyRegistryEntryID`.
#[allow(non_upper_case_globals)]
pub(super) const kIOHIDMapKeyRegistryEntryID: &str = "Registry Entry ID";

/// `kIOHIDSerialNumberKey`.
#[allow(non_upper_case_globals)]
pub(super) const kIOHIDSerialNumberKey: &str = "Serial Number";

/// `kIOHIDProductKey`.
#[allow(non_upper_case_globals)]
pub(super) const kIOHIDProductKey: &str = "Product";

/// USB HID usage page for Keyboard/Keypad.
pub(super) const HID_USAGE_PAGE_KEYBOARD: u32 = 0x07;

/// USB HID usage page for Consumer.
pub(super) const HID_USAGE_PAGE_CONSUMER: u32 = 0x0C;

// ---------------------------------------------------------------------------
// IOReturn constants
// ---------------------------------------------------------------------------

/// `kIOReturnSuccess`.
#[allow(non_upper_case_globals)]
pub(super) const kIOReturnSuccess: u32 = 0;

/// `kIOReturnNotPermitted`.
#[allow(non_upper_case_globals)]
pub(super) const kIOReturnNotPermitted: u32 = 0xe00002c7;

/// `kIOReturnExclusiveAccess`.
#[allow(non_upper_case_globals)]
pub(super) const kIOReturnExclusiveAccess: u32 = 0xe00002b7;

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
pub(super) fn check_io_return(
    result: u32,
    context: &str,
) -> Result<(), IoKitError> {
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

pub(super) unsafe fn IOHIDManagerCreate(
    allocator: CFAllocatorRef,
    options: u32,
) -> *mut IOHIDManager {
    unsafe { (IoKitFunctions::get().manager_create)(allocator, options) }
}

pub(super) unsafe fn IOHIDManagerSetDeviceMatching(
    manager: *mut IOHIDManager,
    matching: CFDictionaryRef,
) {
    unsafe {
        (IoKitFunctions::get().manager_set_device_matching)(manager, matching)
    }
}

pub(super) unsafe fn IOHIDManagerCopyDevices(
    manager: *mut IOHIDManager,
) -> CFSetRef {
    unsafe { (IoKitFunctions::get().manager_copy_devices)(manager) }
}

pub(super) unsafe fn IOHIDManagerScheduleWithRunLoop(
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

pub(super) unsafe fn IOHIDManagerOpen(
    manager: *mut IOHIDManager,
    flags: u32,
) -> u32 {
    unsafe { (IoKitFunctions::get().manager_open)(manager, flags) }
}

pub(super) unsafe fn IOHIDManagerClose(
    manager: *mut IOHIDManager,
    flags: u32,
) -> u32 {
    unsafe { (IoKitFunctions::get().manager_close)(manager, flags) }
}

pub(super) unsafe fn IOHIDDeviceOpen(
    device: *mut IOHIDDevice,
    flags: u32,
) -> u32 {
    unsafe { (IoKitFunctions::get().device_open)(device, flags) }
}

pub(super) unsafe fn IOHIDDeviceClose(
    device: *mut IOHIDDevice,
    flags: u32,
) -> u32 {
    unsafe { (IoKitFunctions::get().device_close)(device, flags) }
}

pub(super) unsafe fn IOHIDDeviceGetProperty(
    device: *mut IOHIDDevice,
    property: CFStringRef,
) -> *const c_void {
    unsafe { (IoKitFunctions::get().device_get_property)(device, property) }
}

pub(super) unsafe fn IOHIDDeviceCopyMatchingElements(
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

pub(super) unsafe fn IOHIDQueueCreate(
    allocator: CFAllocatorRef,
    device: *mut IOHIDDevice,
    depth: CFIndex,
    options: u32,
) -> *mut IOHIDQueue {
    unsafe {
        (IoKitFunctions::get().queue_create)(allocator, device, depth, options)
    }
}

pub(super) unsafe fn IOHIDQueueAddElement(
    queue: *mut IOHIDQueue,
    element: *mut IOHIDElement,
) {
    unsafe { (IoKitFunctions::get().queue_add_element)(queue, element) }
}

pub(super) unsafe fn IOHIDQueueRegisterValueAvailableCallback(
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

pub(super) unsafe fn IOHIDQueueScheduleWithRunLoop(
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

pub(super) unsafe fn IOHIDQueueCopyNextValue(
    queue: *mut IOHIDQueue,
) -> Option<*mut IOHIDValue> {
    let value =
        unsafe { (IoKitFunctions::get().queue_copy_next_value)(queue) };
    (!value.is_null()).then_some(value)
}

pub(super) unsafe fn IOHIDQueueStart(queue: *mut IOHIDQueue) {
    unsafe { (IoKitFunctions::get().queue_start)(queue) }
}

pub(super) unsafe fn IOHIDQueueStop(queue: *mut IOHIDQueue) {
    unsafe { (IoKitFunctions::get().queue_stop)(queue) }
}

pub(super) unsafe fn IOHIDValueGetIntegerValue(
    value: *mut IOHIDValue,
) -> CFIndex {
    unsafe { (IoKitFunctions::get().value_get_integer_value)(value) }
}

pub(super) unsafe fn IOHIDValueGetElement(
    value: *mut IOHIDValue,
) -> *mut IOHIDElement {
    unsafe { (IoKitFunctions::get().value_get_element)(value) }
}

pub(super) unsafe fn IOHIDElementGetUsagePage(
    element: *mut IOHIDElement,
) -> u32 {
    unsafe { (IoKitFunctions::get().element_get_usage_page)(element) }
}

pub(super) unsafe fn IOHIDElementGetUsage(element: *mut IOHIDElement) -> u32 {
    unsafe { (IoKitFunctions::get().element_get_usage)(element) }
}

pub(super) unsafe fn CFArrayGetCount(the_array: *const c_void) -> usize {
    unsafe { (IoKitFunctions::get().cf_array_get_count)(the_array) }
}

pub(super) unsafe fn CFArrayGetValueAtIndex(
    the_array: *const c_void,
    idx: usize,
) -> *const c_void {
    unsafe {
        (IoKitFunctions::get().cf_array_get_value_at_index)(the_array, idx)
    }
}

pub(super) unsafe fn CFSetGetCount(the_set: CFSetRef) -> usize {
    unsafe { (IoKitFunctions::get().cf_set_get_count)(the_set) }
}

pub(super) unsafe fn CFSetApplyFunction(
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

pub(super) unsafe fn CFRelease(cf: *const c_void) {
    unsafe { (IoKitFunctions::get().cf_release)(cf) }
}

pub(super) unsafe fn CFNumberCreate(
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
pub(super) unsafe fn CFNumberGetValue(
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

pub(super) unsafe fn CFDictionaryCreateMutable(
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

pub(super) unsafe fn CFDictionarySetValue(
    dict: *mut c_void,
    key: *const c_void,
    value: *const c_void,
) {
    unsafe { (IoKitFunctions::get().cf_dict_set_value)(dict, key, value) }
}

// ---------------------------------------------------------------------------
// Helper: create a CFString from &str (leaked, caller must CFRelease)
// ---------------------------------------------------------------------------

/// Create a `CFString` from a Rust string slice.  The returned pointer
/// must be released with `CFRelease` when no longer needed.
pub(super) fn create_cf_string(s: &str) -> CFStringRef {
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
