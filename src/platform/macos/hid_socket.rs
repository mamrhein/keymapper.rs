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
//! Communicates with the `KeyMapperDriver` via `IOHIDServiceSocket`.  When
//! the driver is loaded, HID reports are sent to emulate a real hardware
//! keyboard, bypassing the synthetic-event restrictions of `CGEvent`.  When
//! the driver is not available, callers fall back to `CGEvent` posting.

use std::{
    ffi::c_void,
    ptr::{self, NonNull},
};

use objc2_core_graphics::CGKeyCode;

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
            std::mem::transmute::<*mut c_void, Option<FnIOHIDServiceSocketCreate>>(
                libc::dlsym(
                    handle,
                    b"IOHIDServiceSocketCreate\0".as_ptr() as *const _,
                ),
            )
        };
        let send_report = unsafe {
            std::mem::transmute::<
                *mut c_void,
                Option<FnIOHIDServiceSocketSendReport>,
            >(
                libc::dlsym(
                    handle,
                    b"IOHIDServiceSocketSendReport\0".as_ptr() as *const _,
                ),
            )
        };
        let close = unsafe {
            std::mem::transmute::<*mut c_void, Option<FnIOHIDServiceSocketClose>>(
                libc::dlsym(
                    handle,
                    b"IOHIDServiceSocketClose\0".as_ptr() as *const _,
                ),
            )
        };

        // We never call dlclose(), so the IOKit framework handle stays valid
        // for the process lifetime. The raw `*mut c_void` is Copy so no drop
        // guard needs suppression.
        let _ = handle;

        if create.is_none()
            || send_report.is_none()
            || close.is_none()
        {
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
        cached.create.is_some() && cached.send_report.is_some()
            && cached.close.is_some()
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
    /// The given `CGKeyCode` has no known USB HID usage code.
    UnknownKeycode(CGKeyCode),
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
            Self::UnknownKeycode(code) => {
                write!(f, "no USB HID usage code for CGKeyCode {code}")
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
            unsafe { IOServiceMatching(b"KeyMapperDriver\0".as_ptr()) };
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
        let functions = HID_FUNCS.get().expect("HID functions not resolved");
        unsafe {
            (functions.close.unwrap())(self.socket.as_ptr());
        }
    }
}

// ---------------------------------------------------------------------------
// CGKeyCode → USB HID usage code mapping
// ---------------------------------------------------------------------------

/// Map a macOS `CGKeyCode` to its USB HID Keyboard/Keypad usage code.
///
/// Derived from the USB HID Usage Tables v1.21 (page 53–60) cross-referenced
/// with Apple's `ev_keymap.h` virtual key constants.  Returns
/// [`HidSocketError::UnknownKeycode`] for keys without a known HID usage.
pub fn cg_keycode_to_usb_hid(code: CGKeyCode) -> Result<u8, HidSocketError> {
    // Clippy discourages huge match expressions, but a lookup table here is
    // the most efficient and correct approach.
    // Values derived from: Key enum CGKeyCode → HID Usage Tables page 53–60.
    #[allow(clippy::too_many_lines)]
    match code {
        // --- Letters (CGKeyCode from Key enum, HID from USB spec) ---
        0 => Ok(0x04),  // A
        1 => Ok(0x16),  // S
        2 => Ok(0x07),  // D
        3 => Ok(0x09),  // F
        4 => Ok(0x0B),  // H
        5 => Ok(0x0A),  // G
        6 => Ok(0x1D),  // Z
        7 => Ok(0x1B),  // X
        8 => Ok(0x06),  // C
        9 => Ok(0x19),  // V
        10 => Ok(0x63), // IsoExtra (\ | on ISO keyboards)
        11 => Ok(0x05), // B
        12 => Ok(0x14), // Q
        13 => Ok(0x1A), // W
        14 => Ok(0x08), // E
        15 => Ok(0x15), // R
        16 => Ok(0x1C), // Y
        17 => Ok(0x17), // T
        31 => Ok(0x12), // O
        32 => Ok(0x18), // U
        34 => Ok(0x0C), // I
        35 => Ok(0x13), // P
        37 => Ok(0x0F), // L
        38 => Ok(0x0D), // J
        40 => Ok(0x0E), // K
        41 => Ok(0x33), // Semicolon
        45 => Ok(0x11), // N
        46 => Ok(0x10), // M

        // --- Numbers ---
        18 => Ok(0x1E), // 1
        19 => Ok(0x1F), // 2
        20 => Ok(0x20), // 3
        21 => Ok(0x21), // 4
        23 => Ok(0x22), // 5
        22 => Ok(0x23), // 6
        26 => Ok(0x24), // 7
        28 => Ok(0x25), // 8
        25 => Ok(0x26), // 9
        29 => Ok(0x27), // 0

        // --- Edit / navigation ---
        36 => Ok(0x28),  // Return
        51 => Ok(0x2A),  // Backspace
        53 => Ok(0x29),  // Escape
        48 => Ok(0x2B),  // Tab
        49 => Ok(0x2C),  // Space
        117 => Ok(0x4C), // ForwardDelete (HID Clear)

        // --- Modifier keys ---
        59 => Ok(0xE0), // LeftControl
        62 => Ok(0xE1), // RightControl
        56 => Ok(0xE2), // LeftShift
        60 => Ok(0xE3), // RightShift
        58 => Ok(0xE4), // LeftAlt (Option)
        61 => Ok(0xE5), // RightAlt (Right Option)
        55 => Ok(0xE6), // LeftCommand
        54 => Ok(0xE7), // RightCommand
        57 => Ok(0x39), // CapsLock (Keyboard Locking Caps Lock)

        // --- Function keys ---
        122 => Ok(0x3A), // F1
        120 => Ok(0x3B), // F2
        99 => Ok(0x3C),  // F3
        118 => Ok(0x3D), // F4
        96 => Ok(0x3E),  // F5
        97 => Ok(0x3F),  // F6
        98 => Ok(0x40),  // F7
        100 => Ok(0x41), // F8
        101 => Ok(0x42), // F9
        109 => Ok(0x43), // F10
        103 => Ok(0x44), // F11
        111 => Ok(0x45), // F12

        // --- Navigation cluster ---
        115 => Ok(0x4A), // Home
        119 => Ok(0x4D), // End
        116 => Ok(0x4E), // PageUp
        121 => Ok(0x4F), // PageDown
        126 => Ok(0x52), // UpArrow
        125 => Ok(0x51), // DownArrow
        123 => Ok(0x50), // LeftArrow
        124 => Ok(0x4B), // RightArrow

        // --- Punctuation / symbols ---
        27 => Ok(0x2D), // Minus (-)
        24 => Ok(0x2F), // Equal (=)
        33 => Ok(0x31), // BracketLeft ([)
        30 => Ok(0x32), // BracketRight (])
        42 => Ok(0x35), // Backslash (\)
        39 => Ok(0x34), // Quote (' )
        50 => Ok(0x35), // Grave (` ~, HID Non-US # & ~)
        43 => Ok(0x36), // Comma (,)
        47 => Ok(0x38), // Period (.)
        44 => Ok(0x37), // Slash (/)

        // --- Numpad ---
        82 => Ok(0x52), // Numpad0 (Keypad 0)
        83 => Ok(0x53), // Numpad1 (Keypad 1)
        84 => Ok(0x54), // Numpad2 (Keypad 2)
        85 => Ok(0x55), // Numpad3 (Keypad 3)
        86 => Ok(0x56), // Numpad4 (Keypad 4)
        87 => Ok(0x57), // Numpad5 (Keypad 5)
        88 => Ok(0x58), // Numpad6 (Keypad 6)
        89 => Ok(0x59), // Numpad7 (Keypad 7)
        91 => Ok(0x5A), // Numpad8 (Keypad 8)
        92 => Ok(0x5B), // Numpad9 (Keypad 9)
        65 => Ok(0x63), // NumpadDecimal (Keypad .)
        75 => Ok(0x55), // NumpadMultiply (Keypad *)
        69 => Ok(0x5E), // NumpadPlus (Keypad +)
        71 => Ok(0x47), // NumpadClear (Keypad Clear)
        73 => Ok(0x54), // NumpadDivide (Keypad /)
        76 => Ok(0x58), // NumpadEnter (Keypad Enter)
        78 => Ok(0x56), // NumpadMinus (Keypad -)
        90 => Ok(0x59), // NumpadEqual (Keypad =)

        // --- Extended function keys (F13–F20) ---
        105 => Ok(0x68), // F13 (Execute)
        107 => Ok(0x69), // F14 (Help)
        113 => Ok(0x6A), // F15 (Menu / Select)
        106 => Ok(0x6B), // F16 (Stop)
        110 => Ok(0x6C), // F17 (Again / Undo)
        104 => Ok(0x6D), // F18 (Find / Open)
        102 => Ok(0x6E), // F19 (Cut)

        _ => Err(HidSocketError::UnknownKeycode(code)),
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
/// For a key-down event, the USB HID usage code is placed in slot 0
/// (byte 3).  For a key-up event, all key slots are cleared to zero.
pub fn build_keyboard_report(
    modifiers: u8,
    code: Option<CGKeyCode>,
) -> Result<[u8; 9], HidSocketError> {
    let mut report = [0u8; 9];
    report[0] = 1; // Report ID
    report[1] = modifier_to_hid(modifiers);

    if let Some(key_code) = code {
        report[3] = cg_keycode_to_usb_hid(key_code)?;
    }

    Ok(report)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modifier_to_hid_passthrough() {
        assert_eq!(modifier_to_hid(0x00), 0x00);
        assert_eq!(modifier_to_hid(0x01), 0x01); // LeftControl
        assert_eq!(modifier_to_hid(0x10), 0x10); // LeftAlt
        assert_eq!(modifier_to_hid(0x81), 0x81); // RightCommand + LeftControl
    }

    #[test]
    fn test_letter_keycodes() {
        assert_eq!(cg_keycode_to_usb_hid(0).unwrap(), 0x04); // A
        assert_eq!(cg_keycode_to_usb_hid(6).unwrap(), 0x1D); // Z
    }

    #[test]
    fn test_number_keycodes() {
        assert_eq!(cg_keycode_to_usb_hid(18).unwrap(), 0x1E); // Number1
        assert_eq!(cg_keycode_to_usb_hid(29).unwrap(), 0x27); // Number0
    }

    #[test]
    fn test_modifier_keycodes() {
        assert_eq!(cg_keycode_to_usb_hid(59).unwrap(), 0xE0); // LeftControl
        assert_eq!(cg_keycode_to_usb_hid(55).unwrap(), 0xE6); // LeftCommand
        assert_eq!(cg_keycode_to_usb_hid(57).unwrap(), 0x39); // CapsLock
    }

    #[test]
    fn test_arrow_keycodes() {
        assert_eq!(cg_keycode_to_usb_hid(126).unwrap(), 0x52); // UpArrow
        assert_eq!(cg_keycode_to_usb_hid(125).unwrap(), 0x51); // DownArrow
        assert_eq!(cg_keycode_to_usb_hid(123).unwrap(), 0x50); // LeftArrow
        assert_eq!(cg_keycode_to_usb_hid(124).unwrap(), 0x4B); // RightArrow
    }

    #[test]
    fn test_unknown_keycode() {
        assert!(cg_keycode_to_usb_hid(70).is_err());
    }

    #[test]
    fn test_build_report_key_down() {
        let report = build_keyboard_report(0x40, Some(0)).unwrap();
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
}
