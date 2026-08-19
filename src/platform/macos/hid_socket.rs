// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! User-space bridge to the DriverKit virtual HID keyboard driver.
//!
//! Communicates with the `KeyMapperDriver` via `IOHIDServiceSocket`.  HID
//! reports are sent to emulate a real hardware keyboard, bypassing the
//! synthetic-event restrictions of `CGEvent`.

use std::{
    ffi::c_void,
    ptr::{self, NonNull},
};

// ---------------------------------------------------------------------------
// IOKit FFI declarations (linked via IOKit framework)
// ---------------------------------------------------------------------------

/// Opaque IOKit object handle.
#[allow(non_camel_case_types)]
pub type io_object_t = u32;

/// Sentinel for a null `io_object_t`.
const MACH_PORT_NULL: io_object_t = 0;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const u8) -> io_object_t;
    fn IOServiceGetMatchingServices(
        _main_port: io_object_t,
        matching: io_object_t,
        existing: *mut io_object_t,
    ) -> io_object_t;
    fn IOObjectRelease(obj: io_object_t);
    fn IOIteratorNext(iterator: io_object_t) -> io_object_t;
    fn IORegistryEntryCreateCFProperty(
        entry: io_object_t,
        key: *const u8,
        allocator: io_object_t,
        options: u32,
    ) -> io_object_t;
}

/// `kCFNumberSInt32Type` — CFNumber type for 32-bit signed integers.
#[allow(non_upper_case_globals)]
const kCFNumberSInt32Type: u32 = 3;

// Resolve `CFNumberGetValue` from CoreFoundation at runtime.
// CFNumberGetValue is part of CoreFoundation, already linked by
// objc2-core-foundation.
#[allow(improper_ctypes_definitions)]
unsafe extern "C" {
    fn CFNumberGetValue(
        num: io_object_t,
        the_type: u32,
        value_ptr: *mut c_void,
    ) -> bool;
}

// ---------------------------------------------------------------------------
// IOHIDServiceSocket — resolved dynamically via dlsym
// ---------------------------------------------------------------------------

/// Opaque `IOHIDServiceSocketRef` from IOKit/hid/IOHIDServiceSocket.h.
#[repr(C)]
pub struct IOHIDServiceSocket {
    _private: [u8; 0],
}

/// Function pointer for `IOHIDServiceSocketCreate`.
type FnIOHIDServiceSocketCreate = unsafe extern "C" fn(
    service: io_object_t,
    product_id: u32,
    vendor_id: u32,
    socket: *mut *mut IOHIDServiceSocket,
) -> io_object_t;

/// Function pointer for `IOHIDServiceSocketSendReport`.
type FnIOHIDServiceSocketSendReport = unsafe extern "C" fn(
    socket: *mut IOHIDServiceSocket,
    report: *const u8,
    length: usize,
) -> io_object_t;

/// Function pointer for `IOHIDServiceSocketClose`.
type FnIOHIDServiceSocketClose =
    unsafe extern "C" fn(socket: *mut IOHIDServiceSocket);

/// Resolved function pointers for the `IOHIDServiceSocket` API.  Cached in a
/// static so they can be shared across multiple `HidSocket` instances.
static HID_FUNCS: std::sync::OnceLock<HidFunctions> =
    std::sync::OnceLock::new();

struct HidFunctions {
    create: Option<FnIOHIDServiceSocketCreate>,
    send_report: Option<FnIOHIDServiceSocketSendReport>,
    close: Option<FnIOHIDServiceSocketClose>,
}

impl HidFunctions {
    /// Try to resolve all `IOHIDServiceSocket*` symbols from the IOKit
    /// framework at runtime.  Resolves once and caches the result globally.
    fn resolve() -> bool {
        if HID_FUNCS.get().is_some() {
            return true;
        }

        // Load the IOKit framework dynamically.
        let path = b"/System/Library/Frameworks/IOKit.framework/IOKit\0";
        let handle = unsafe { libc::dlopen(path.as_ptr() as *const _, 2) };
        if handle.is_null() {
            return false;
        }

        // SAFETY: `Option<FnType>` uses niche optimization where null pointer
        // bits represent `None`. Transmuting `*mut c_void` (from dlsym) to
        // `Option<FnType>` is valid because both have identical size and
        // alignment, and the null/non-null bit patterns match.
        let create = unsafe {
            std::mem::transmute::<
                *mut c_void,
                Option<FnIOHIDServiceSocketCreate>,
            >(libc::dlsym(
                handle,
                c"IOHIDServiceSocketCreate".as_ptr(),
            ))
        };
        let send_report = unsafe {
            std::mem::transmute::<
                *mut c_void,
                Option<FnIOHIDServiceSocketSendReport>,
            >(libc::dlsym(
                handle,
                c"IOHIDServiceSocketSendReport".as_ptr(),
            ))
        };
        let close = unsafe {
            std::mem::transmute::<*mut c_void, Option<FnIOHIDServiceSocketClose>>(
                libc::dlsym(
                    handle,
                    c"IOHIDServiceSocketClose".as_ptr(),
                ),
            )
        };

        // We never call dlclose(), so the IOKit framework handle stays valid
        // for the process lifetime. The raw `*mut c_void` is Copy so no drop
        // guard needs suppression.
        let _ = handle;

        if create.is_none() || send_report.is_none() || close.is_none() {
            return false;
        }

        let success = HID_FUNCS
            .set(HidFunctions {
                create,
                send_report,
                close,
            })
            .is_ok();

        if success {
            return true;
        }

        // Another thread raced us. Verify the cached values are valid.
        let cached = HID_FUNCS.get().expect("race in HidFunctions::resolve");
        cached.create.is_some()
            && cached.send_report.is_some()
            && cached.close.is_some()
    }

    /// Return the resolved function pointers.
    ///
    /// Panics if resolution failed. Callers must ensure `resolve()` succeeded
    /// before calling this.
    fn get() -> &'static Self {
        HID_FUNCS
            .get()
            .expect("HID functions not resolved. Call resolve() first.")
    }
}

/// Default vendor/product IDs used by the DriverKit virtual keyboard.
const DEFAULT_VENDOR_ID: u32 = 0xFFF0;
const DEFAULT_PRODUCT_ID: u32 = 0x1001;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors that can occur during HID socket operations.
#[derive(Debug)]
pub enum HidSocketError {
    /// The virtual HID driver is not loaded or discoverable via IOKit.
    DriverNotFound,
    /// The `IOHIDServiceSocket*` symbols are not available in the IOKit
    /// framework on this system.
    SocketApiUnavailable,
    /// Failed to open the `IOHIDServiceSocket`.
    #[allow(dead_code)]
    SocketOpenFailed(u32),
    /// Failed to send an HID report through the socket.
    SendFailed(u32),
    /// Consumer Page usage has no mapping to a consumer report.
    UnknownConsumerUsage(u16),
}

impl std::fmt::Display for HidSocketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DriverNotFound => {
                write!(f, "virtual HID driver not found in IOKit registry")
            }
            Self::SocketApiUnavailable => {
                write!(
                    f,
                    "IOHIDServiceSocket API not available (symbols \
                     IOHIDServiceSocketCreate, IOHIDServiceSocketSendReport, \
                     IOHIDServiceSocketClose not found in IOKit framework)"
                )
            }
            Self::SocketOpenFailed(status) => {
                write!(
                    f,
                    "IOHIDServiceSocketCreate failed with status {status:#x}"
                )
            }
            Self::SendFailed(status) => {
                write!(
                    f,
                    "IOHIDServiceSocketSendReport failed with status \
                     {status:#x}"
                )
            }
            Self::UnknownConsumerUsage(usage) => {
                write!(
                    f,
                    "no consumer report mapping for Consumer Page usage \
                     {usage:#x}"
                )
            }
        }
    }
}

impl std::error::Error for HidSocketError {}

// ---------------------------------------------------------------------------
// HidSocket — safe wrapper around IOHIDServiceSocket
// ---------------------------------------------------------------------------

/// A connection to the DriverKit virtual HID keyboard driver.
///
/// Created via [`HidSocket::discover_and_open`].  Sending reports is done
/// through [`HidSocket::send_report`].
pub struct HidSocket {
    socket: NonNull<IOHIDServiceSocket>,
}

impl HidSocket {
    /// Discover the virtual HID driver in IOKit and open a socket to it.
    ///
    /// Enumerates services matching the expected driver class name.  If no
    /// matching service is found, returns [`HidSocketError::DriverNotFound`].
    pub fn discover_and_open() -> Result<Self, HidSocketError> {
        // Resolve the IOHIDServiceSocket* symbols at runtime.
        if !HidFunctions::resolve() {
            return Err(HidSocketError::SocketApiUnavailable);
        }
        let functions = HID_FUNCS.get().expect("HID functions not resolved");

        let mut iterator: io_object_t = MACH_PORT_NULL;

        // Build a matching dictionary for our driver class name.
        let matching =
            unsafe { IOServiceMatching(c"KeyMapperDriver".as_ptr() as *const u8) };
        if matching == MACH_PORT_NULL {
            return Err(HidSocketError::DriverNotFound);
        }

        // kIOMasterPortDefault is 0.
        let master_port: io_object_t = MACH_PORT_NULL;
        let kern_return = unsafe {
            IOServiceGetMatchingServices(master_port, matching, &mut iterator)
        };

        if kern_return != 0 {
            unsafe { IOObjectRelease(iterator) };
            return Err(HidSocketError::DriverNotFound);
        }

        let mut result = Err(HidSocketError::DriverNotFound);

        // Iterate over all matching services.
        loop {
            let service = unsafe { IOIteratorNext(iterator) };
            if service == MACH_PORT_NULL {
                break;
            }

            // Try to read vendor/product IDs from the service's registry
            // properties; fall back to defaults if not present.
            let vendor_id =
                Self::get_property_u32(service, b"kUSBHIDVendorId\0")
                    .unwrap_or(DEFAULT_VENDOR_ID);
            let product_id =
                Self::get_property_u32(service, b"kUSBHIDProductId\0")
                    .unwrap_or(DEFAULT_PRODUCT_ID);

            // Attempt to create the socket.
            let mut socket: *mut IOHIDServiceSocket = ptr::null_mut();
            let status = unsafe {
                (functions.create.unwrap())(
                    service,
                    product_id,
                    vendor_id,
                    &mut socket as *mut _,
                )
            };

            if status == 0 && !socket.is_null() {
                eprintln!(
                    "HID driver connected: vendor={vendor_id:#x}, \
                     product={product_id:#x}"
                );
                result = Ok(Self {
                    socket: unsafe { NonNull::new_unchecked(socket) },
                });
            } else {
                eprintln!(
                    "IOHIDServiceSocketCreate failed for service \
                     (vendor={vendor_id:#x}, product={product_id:#x}): \
                     status={status:#x}"
                );
            }

            unsafe { IOObjectRelease(service) };

            // Stop after the first successful connection.
            if result.is_ok() {
                break;
            }
        }

        unsafe { IOObjectRelease(iterator) };
        result
    }

    /// Read a 32-bit unsigned integer property from an IOKit registry entry.
    fn get_property_u32(entry: io_object_t, key: &[u8]) -> Option<u32> {
        let num = unsafe {
            IORegistryEntryCreateCFProperty(
                entry,
                key.as_ptr(),
                MACH_PORT_NULL,
                0,
            )
        };
        if num == MACH_PORT_NULL {
            return None;
        }

        let mut value: u32 = 0;
        let success = unsafe {
            CFNumberGetValue(
                num,
                kCFNumberSInt32Type,
                &mut value as *mut _ as *mut c_void,
            )
        };

        unsafe { IOObjectRelease(num) };

        if success { Some(value) } else { None }
    }

    /// Send a raw HID report through the socket.
    ///
    /// The caller is responsible for constructing a valid report that matches
    /// the driver's report descriptor.  For the standard keyboard boot
    /// protocol this is a 9-byte report: report ID (1), modifier byte,
    /// reserved byte, and 6 key-code slots.
    pub fn send_report(&self, report: &[u8]) -> Result<(), HidSocketError> {
        let functions = HID_FUNCS.get().expect("HID functions not resolved");
        let status = unsafe {
            (functions.send_report.unwrap())(
                self.socket.as_ptr(),
                report.as_ptr(),
                report.len(),
            )
        };

        if status == 0 {
            Ok(())
        } else {
            Err(HidSocketError::SendFailed(status))
        }
    }
}

impl Drop for HidSocket {
    fn drop(&mut self) {
        unsafe {
            let close = HidFunctions::get().close
                .expect("HID functions not resolved");
            close(self.socket.as_ptr());
        }
    }
}

// ---------------------------------------------------------------------------
// Modifier conversion
// ---------------------------------------------------------------------------

/// Convert our internal modifier bitmask to a USB HID modifier byte.
///
/// Our `ModifierRole` discriminants (bit positions) map directly to the HID
/// modifier byte bit positions:
///
/// | Bit position | ModifierRole   | HID modifier bit |
/// |--------------|----------------|------------------|
/// | 0            | LeftControl    | bit 0 = 0x01     |
/// | 1            | RightControl   | bit 1 = 0x02     |
/// | 2            | LeftShift      | bit 2 = 0x04     |
/// | 3            | RightShift     | bit 3 = 0x08     |
/// | 4            | LeftAlt        | bit 4 = 0x10     |
/// | 5            | RightAlt       | bit 5 = 0x20     |
/// | 6            | LeftCommand    | bit 6 = 0x40     |
/// | 7            | RightCommand   | bit 7 = 0x80     |
///
/// Because the bit positions line up exactly, the raw `u8` value is already
/// a valid HID modifier byte.
#[inline]
pub fn modifier_to_hid(modifiers: u8) -> u8 {
    modifiers
}

// ---------------------------------------------------------------------------
// HID report construction
// ---------------------------------------------------------------------------

/// Build a standard USB keyboard HID report for a single key event.
///
/// Layout (9 bytes):
/// - Byte 0: Report ID (1)
/// - Byte 1: Modifier bitmask
/// - Byte 2: Reserved (always 0)
/// - Bytes 3–8: Key codes (6 slots; 0x00 = no key pressed)
///
/// For a key-down event, the raw USB HID usage byte is placed in slot 0
/// (byte 3).  For a key-up event, all key slots are cleared to zero.
pub fn build_keyboard_report(
    modifiers: u8,
    code: Option<u8>,
) -> Result<[u8; 9], HidSocketError> {
    let mut report = [0u8; 9];
    report[0] = 1; // Report ID
    report[1] = modifier_to_hid(modifiers);

    if let Some(usage_byte) = code {
        report[3] = usage_byte;
    }

    Ok(report)
}

/// Build a Consumer Page HID report for media/display control keys.
///
/// Constructs a raw HID report matching the Consumer Page collection
/// declared in the DriverKit descriptor.  Report format:
/// `[report_id=2, usage_lo, usage_hi]` for a 16-bit usage field.
///
/// Returns [`HidSocketError::UnknownConsumerUsage`] for usages without
/// a known mapping.
pub fn build_consumer_report(
    usage_id: u16,
) -> Result<[u8; 3], HidSocketError> {
    // Validate that the usage_id is a known Consumer Page usage.
    // This check ensures we only send valid consumer controls to the
    // DriverKit driver.
    let _ = match usage_id {
        // Media controls
        0xCD | // Play/Pause
        0xE9 | // Volume Up
        0xEA | // Volume Down
        0xE2 | // Mute
        0xB5 | // Next Track
        0xB6 | // Previous Track
        0xB7 | // Stop
        // Display controls
        0x6F | // Brightness Up
        0x70 => Ok(()), // Brightness Down
        _ => Err(HidSocketError::UnknownConsumerUsage(usage_id)),
    }?;

    let mut report = [0u8; 3];
    report[0] = 2; // Report ID for consumer page
    report[1] = (usage_id & 0xFF) as u8; // Usage low byte
    report[2] = ((usage_id >> 8) & 0xFF) as u8; // Usage high byte

    Ok(report)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Modifier conversion
    // -----------------------------------------------------------------------

    #[test]
    fn test_modifier_to_hid_passthrough() {
        assert_eq!(modifier_to_hid(0x00), 0x00);
        assert_eq!(modifier_to_hid(0x01), 0x01); // LeftControl
        assert_eq!(modifier_to_hid(0x10), 0x10); // LeftAlt
        assert_eq!(modifier_to_hid(0x81), 0x81); // RightCommand + LeftControl
    }

    #[test]
    fn test_modifier_to_hid_all_bits() {
        // Verify every modifier bit position maps correctly.
        for bit in 0..8 {
            let mask = 1u8 << bit;
            assert_eq!(
                modifier_to_hid(mask),
                mask,
                "bit {bit} should pass through unchanged"
            );
        }
        // All modifiers pressed.
        assert_eq!(modifier_to_hid(0xFF), 0xFF);
    }

    // -----------------------------------------------------------------------
    // Modifier conversion
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_report_key_down() {
        let report = build_keyboard_report(0x40, Some(0x04)).unwrap();
        assert_eq!(report[0], 1); // Report ID
        assert_eq!(report[1], 0x40); // LeftCommand modifier (bit 6)
        assert_eq!(report[2], 0); // Reserved
        assert_eq!(report[3], 0x04); // 'A' usage code
        assert_eq!(report[4..], [0u8; 5]); // Remaining slots empty
    }

    #[test]
    fn test_build_report_key_up() {
        let report = build_keyboard_report(0x40, None).unwrap();
        assert_eq!(report[1], 0x40); // Modifier still held
        assert_eq!(report[3..], [0u8; 6]); // All key slots cleared
    }

    #[test]
    fn test_build_report_no_modifiers() {
        let report = build_keyboard_report(0x00, Some(0x2C)).unwrap(); // Space
        assert_eq!(report[0], 1);
        assert_eq!(report[1], 0x00); // No modifiers
        assert_eq!(report[2], 0);
        assert_eq!(report[3], 0x2C); // Space usage code
    }

    #[test]
    fn test_build_report_all_modifiers() {
        let report = build_keyboard_report(0xFF, Some(0x04)).unwrap();
        assert_eq!(report[1], 0xFF);
        assert_eq!(report[3], 0x04); // 'A'
    }

    #[test]
    fn test_build_report_empty_all_clear() {
        // A fully empty report (key up, no modifiers).
        let report = build_keyboard_report(0x00, None).unwrap();
        assert_eq!(report, [1, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    // -----------------------------------------------------------------------
    // Chord emission report sequence (mocked logic)
    // -----------------------------------------------------------------------

    /// Simulate the sequence of HID reports produced by emitting a chord
    /// like Cmd+A: the modifier is held while the key is pressed.
    #[test]
    fn test_chord_cmd_a_report_sequence() {
        // Cmd (bit 6 = 0x40) + A (HID usage 0x04).
        let report = build_keyboard_report(0x40, Some(0x04)).unwrap();
        assert_eq!(report[1], 0x40); // Cmd held
        assert_eq!(report[3], 0x04); // A pressed

        // Key release: modifiers still held, key slots cleared.
        let report_up = build_keyboard_report(0x40, None).unwrap();
        assert_eq!(report_up[1], 0x40); // Cmd still held
        assert_eq!(report_up[3], 0x00); // Key released

        // Modifier release: all clear.
        let report_mod_up = build_keyboard_report(0x00, None).unwrap();
        assert_eq!(report_mod_up[1], 0x00);
    }

    /// Simulate the report sequence for Ctrl+Shift+A.
    #[test]
    fn test_chord_ctrl_shift_a_report_sequence() {
        // Ctrl (bit 0 = 0x01) + Shift (bit 2 = 0x04) = 0x05.
        let modifiers = 0x05;
        let report = build_keyboard_report(modifiers, Some(0x04)).unwrap();
        assert_eq!(report[1], 0x05);
        assert_eq!(report[3], 0x04); // A
    }
}
