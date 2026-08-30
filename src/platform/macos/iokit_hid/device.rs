// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Device and queue abstractions over the raw IOHIDLib bindings.
//!
//! [`HidDeviceManager`] discovers keyboards via `IOHIDManager` (matching
//! only, no input callbacks); [`HidDevice`] wraps a single discovered
//! device; [`HidQueue`] receives raw HID values from a seized device, and
//! [`HidQueueHandle`] keeps a queue and its callback context alive.

use std::{ffi::c_void, ptr};

use objc2_core_foundation::{
    CFIndex, CFRunLoop, CFString, CFStringBuiltInEncodings,
    kCFRunLoopDefaultMode,
};

use super::{
    capture::{HidQueueContext, hid_queue_value_callback},
    ffi::{
        CFArrayGetCount, CFArrayGetValueAtIndex, CFDictionaryCreateMutable,
        CFDictionarySetValue, CFNumberCreate, CFNumberGetValue, CFNumberRef,
        CFRelease, CFSetApplyFunction, CFSetGetCount, IOHIDDevice,
        IOHIDDeviceClose, IOHIDDeviceCopyMatchingElements,
        IOHIDDeviceGetProperty, IOHIDDeviceOpen, IOHIDElement, IOHIDManager,
        IOHIDManagerClose, IOHIDManagerCopyDevices, IOHIDManagerCreate,
        IOHIDManagerOpen, IOHIDManagerScheduleWithRunLoop, IOHIDQueue,
        IOHIDQueueAddElement, IOHIDQueueCreate,
        IOHIDQueueRegisterValueAvailableCallback,
        IOHIDQueueScheduleWithRunLoop, IOHIDQueueStart, IOHIDQueueStop,
        IoKitError, check_io_return, create_cf_string, kCFAllocatorDefault,
        kCFNumberSInt32Type, kIOHIDMapKeyProductID, kIOHIDMapKeyVendorID,
        kIOHIDOptionsTypeNone, kIOHIDOptionsTypeSeizeDevice, kIOHIDProductKey,
    },
};
use crate::platform::macos::{
    INJECTION_KEYBOARD_IDENTITY, KeyboardIdentity, OUTPUT_KEYBOARD_IDENTITY,
};

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
            .map(crate::platform::macos::keyboard::vendor_id_to_name)
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
