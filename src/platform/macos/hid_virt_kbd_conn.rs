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
//! Communicates with the `KeyMapperDriver` over IOKit's Mach-message
//! abstraction: the driver service is opened with `IOServiceOpen()` and HID
//! reports are sent through `IOConnectCallMethod()`.  The driver feeds each
//! report into the HID event system, emulating a real hardware keyboard and
//! bypassing the synthetic-event restrictions of `CGEvent`.

use std::{
    ffi::c_void,
    ptr, thread,
    time::{Duration, Instant},
};

// ---------------------------------------------------------------------------
// IOKit FFI declarations (linked via IOKit framework)
// ---------------------------------------------------------------------------

/// Opaque IOKit object handle.
#[allow(non_camel_case_types)]
type io_object_t = u32;

/// Opaque IOKit connection handle.
#[allow(non_camel_case_types)]
type io_connect_t = u32;

/// Opaque CoreFoundation object handle (a 64-bit pointer).
///
/// `IOServiceMatching()` returns a CFDictionary, not an IOKit registry
/// object, so it must be handled as a full-width pointer.  Truncating it to
/// 32 bits yields an invalid address that crashes inside IOKit.
#[allow(non_camel_case_types)]
type cf_object_t = *mut c_void;

/// Sentinel for a null `io_object_t`.
const MACH_PORT_NULL: io_object_t = 0;

/// `MACH_TASK_SELF` — the well-known port name of the calling task, passed
/// to `IOServiceOpen()`.  The kernel resolves port name 3 in the caller's
/// Mach port space to the calling task's own task port.
const MACH_TASK_SELF: u32 = 0x00000003;

/// Selector for sending an HID input report to the driver.
///
/// Mirrors `kKeyMapperSendReportSelector` in
/// `driver/KeyMapperVirtualHID/KeyMapperProtocol.h`.  Any change there must
/// be reflected here.
const K_KEYMAPPER_SEND_REPORT_SELECTOR: u32 = 0;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    /// Returns a retained CFDictionary (a 64-bit pointer), or null on
    /// allocation failure.
    fn IOServiceMatching(name: *const u8) -> cf_object_t;
    /// Consumes one reference to `matching` (it is CFReleased internally),
    /// so the caller must not release it again.
    fn IOServiceGetMatchingServices(
        _main_port: io_object_t,
        matching: cf_object_t,
        existing: *mut io_object_t,
    ) -> i32;
    fn IOObjectRelease(obj: io_object_t);
    fn IOIteratorNext(iterator: io_object_t) -> io_object_t;
    fn IOServiceOpen(
        service: io_object_t,
        task: u32,
        type_: u32,
        connect: *mut io_connect_t,
    ) -> i32;
    fn IOServiceClose(connect: io_connect_t) -> i32;
    /// Extended 10-argument form of `IOConnectCallMethod()`.  The struct
    /// counts are byte counts, not word counts.  Null `output`/`output_cnt`
    /// and null `output_struct`/`output_struct_cnt` are accepted.
    fn IOConnectCallMethod(
        connect: io_connect_t,
        selector: u32,
        input: *const u64,
        input_cnt: u32,
        input_struct: *const c_void,
        input_struct_cnt: usize,
        output: *mut u64,
        output_cnt: *mut u32,
        output_struct: *mut c_void,
        output_struct_cnt: *mut usize,
    ) -> i32;
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors that can occur while communicating with the virtual HID driver.
#[derive(Debug)]
pub enum HidVirtKbdConnError {
    /// The virtual HID driver is not loaded or discoverable via IOKit.
    DriverNotFound,
    /// A matching driver service was found, but `IOServiceOpen()` failed.
    ConnectOpenFailed(u32),
    /// Failed to send an HID report through the connection.
    SendFailed(u32),
    /// Consumer Page usage has no mapping to a consumer report.
    UnknownConsumerUsage(u16),
}

impl std::fmt::Display for HidVirtKbdConnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DriverNotFound => {
                write!(f, "virtual HID driver not found in IOKit registry")
            }
            Self::ConnectOpenFailed(status) => {
                write!(
                    f,
                    "IOServiceOpen failed for the virtual HID driver (status \
                     {status:#x})"
                )
            }
            Self::SendFailed(status) => {
                write!(
                    f,
                    "IOConnectCallMethod failed for the virtual HID driver \
                     (status {status:#x})"
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

impl std::error::Error for HidVirtKbdConnError {}

// ---------------------------------------------------------------------------
// HidVirtKbdConn — safe wrapper around an IOService connection
// ---------------------------------------------------------------------------

/// A connection to the DriverKit virtual HID keyboard driver.
///
/// Created via [`HidVirtKbdConn::discover_and_open`].  Sending reports is done
/// through [`HidVirtKbdConn::send_report`].
pub struct HidVirtKbdConn {
    connect: io_connect_t,
}

/// Maximum time to wait for the DriverKit driver to become available.
///
/// On a fresh CI runner, the DriverKit extension may take a few seconds to
/// load into the IOKit registry after installation.  This timeout gives it
/// time to appear before giving up.  Override with the
/// `KEYMAPPER_HID_DRIVER_TIMEOUT` environment variable (in seconds); set to
/// 0 to fail fast.
const DRIVER_WAIT_TIMEOUT_SECS: u64 = 15;

/// Interval between driver-availability retries.
const DRIVER_WAIT_INTERVAL: Duration = Duration::from_millis(500);

impl HidVirtKbdConn {
    /// Discover the virtual HID driver in IOKit and open a connection to it.
    ///
    /// Enumerates services matching the expected driver class name and
    /// retries until one accepts an `IOServiceOpen()` connection or the wait
    /// timeout elapses.  If no matching service is found within the timeout,
    /// returns [`HidVirtKbdConnError::DriverNotFound`].
    pub fn discover_and_open() -> Result<Self, HidVirtKbdConnError> {
        // Determine the wait timeout.  Default to DRIVER_WAIT_TIMEOUT_SECS,
        // but allow override via environment variable (0 = fail fast).
        let timeout_secs = std::env::var("KEYMAPPER_HID_DRIVER_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DRIVER_WAIT_TIMEOUT_SECS);

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);

        loop {
            match Self::try_discover_and_open() {
                Ok(conn) => return Ok(conn),
                Err(e) => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    eprintln!(
                        "HID driver not ready ({e}); retrying in {} ms...",
                        DRIVER_WAIT_INTERVAL.as_millis()
                    );
                    thread::sleep(DRIVER_WAIT_INTERVAL);
                }
            }
        }
    }

    /// Attempt to discover the driver and open a connection, without
    /// retrying.
    fn try_discover_and_open() -> Result<Self, HidVirtKbdConnError> {
        let mut iterator: io_object_t = MACH_PORT_NULL;

        // Build a matching dictionary for our driver class name.  This is a
        // retained CFDictionary (64-bit pointer), not an IOKit registry
        // object.
        let matching = unsafe {
            IOServiceMatching(c"KeyMapperDriver".as_ptr() as *const u8)
        };
        if matching.is_null() {
            return Err(HidVirtKbdConnError::DriverNotFound);
        }

        // kIOMasterPortDefault is 0.
        let master_port: io_object_t = MACH_PORT_NULL;
        // IOServiceGetMatchingServices() consumes the reference to
        // `matching`, so it must not be released again here.
        let kern_return = unsafe {
            IOServiceGetMatchingServices(master_port, matching, &mut iterator)
        };

        if kern_return != 0 {
            // The iterator is only valid on success; guard the release.
            if iterator != MACH_PORT_NULL {
                unsafe { IOObjectRelease(iterator) };
            }
            return Err(HidVirtKbdConnError::DriverNotFound);
        }

        let mut result = Err(HidVirtKbdConnError::DriverNotFound);
        // The last non-zero status from IOServiceOpen(), if any service was
        // found but none could be opened.
        let mut open_status: Option<i32> = None;

        // Iterate over all matching services.
        loop {
            let service = unsafe { IOIteratorNext(iterator) };
            if service == MACH_PORT_NULL {
                break;
            }

            // Open a connection to the driver.  The driver creates a
            // KeyMapperUserClient for this connection (see
            // KeyMapperDriver::NewUserClient), which receives reports via
            // IOConnectCallMethod().  The driver ignores the connection
            // type, so 0 is passed.
            let mut connect: io_connect_t = 0;
            let status = unsafe {
                IOServiceOpen(service, MACH_TASK_SELF, 0, &mut connect)
            };

            if status == 0 {
                eprintln!("HID driver connected");
                result = Ok(Self { connect });
            } else {
                eprintln!(
                    "IOServiceOpen failed for service: status={status:#x}"
                );
                open_status = Some(status);
            }

            unsafe { IOObjectRelease(service) };

            // Stop after the first successful connection.
            if result.is_ok() {
                break;
            }
        }

        unsafe { IOObjectRelease(iterator) };

        // If a service was found but no connection could be opened, report
        // the open failure instead of a misleading "driver not found".
        if result.is_err()
            && let Some(status) = open_status
        {
            return Err(HidVirtKbdConnError::ConnectOpenFailed(status as u32));
        }

        result
    }

    /// Send a raw HID report to the driver.
    ///
    /// The report bytes are passed as the structure input of an
    /// `IOConnectCallMethod()` call with the send-report selector.  The
    /// caller is responsible for constructing a valid report that matches
    /// the driver's report descriptor.  For the standard keyboard boot
    /// protocol this is a 9-byte report: report ID (1), modifier byte,
    /// reserved byte, and 6 key-code slots.
    pub fn send_report(
        &self,
        report: &[u8],
    ) -> Result<(), HidVirtKbdConnError> {
        // The report bytes are passed as the structure input.  The scalar
        // input/output arrays and the structure output are unused, so they
        // are passed as null (the IOKit library accepts null for all of
        // them).  `input_struct_cnt` is a byte count.
        let status = unsafe {
            IOConnectCallMethod(
                self.connect,
                K_KEYMAPPER_SEND_REPORT_SELECTOR,
                ptr::null(),
                0,
                report.as_ptr() as *const c_void,
                report.len(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };

        if status == 0 {
            Ok(())
        } else {
            Err(HidVirtKbdConnError::SendFailed(status as u32))
        }
    }
}

impl Drop for HidVirtKbdConn {
    fn drop(&mut self) {
        unsafe { IOServiceClose(self.connect) };
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
/// | 1            | LeftShift      | bit 1 = 0x02     |
/// | 2            | LeftAlt        | bit 2 = 0x04     |
/// | 3            | LeftCommand    | bit 3 = 0x08     |
/// | 4            | RightControl   | bit 4 = 0x10     |
/// | 5            | RightShift     | bit 5 = 0x20     |
/// | 6            | RightAlt       | bit 6 = 0x40     |
/// | 7            | RightCommand   | bit 7 = 0x80     |
///
/// Because the bit positions line up exactly, the raw `u8` value is already
/// a valid HID modifier byte.
#[cfg(test)]
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
#[cfg(test)]
pub fn build_keyboard_report(
    modifiers: u8,
    code: Option<u8>,
) -> Result<[u8; 9], HidVirtKbdConnError> {
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
/// `[report_id=2, press_lo, press_hi, release_lo, release_hi]` for two
/// 16-bit usage fields (press and release).
///
/// The usage is placed in the press field; the release field is left zero.
/// To release the key, send an all-clear report (see
/// [`build_consumer_release_report`]).
///
/// Returns [`HidVirtKbdConnError::UnknownConsumerUsage`] for usages without
/// a known mapping.
#[cfg(test)]
pub fn build_consumer_report(
    usage_id: u16,
) -> Result<[u8; 5], HidVirtKbdConnError> {
    // Validate that the usage_id is a known Consumer Page usage.
    // This check ensures we only send valid consumer controls to the
    // DriverKit driver.
    match usage_id {
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
        _ => Err(HidVirtKbdConnError::UnknownConsumerUsage(usage_id)),
    }?;

    let mut report = [0u8; 5];
    report[0] = 2; // Report ID for consumer page
    report[1] = (usage_id & 0xFF) as u8; // Press field, low byte
    report[2] = ((usage_id >> 8) & 0xFF) as u8; // Press field, high byte
    // Release field (bytes 3--4) stays zero.

    Ok(report)
}

/// Build an all-clear Consumer Page HID report to release a consumer key.
///
/// Report format: `[report_id=2, 0, 0, 0, 0]` — both the press and release
/// 16-bit usage fields are zero, which releases any held consumer key.
#[cfg(test)]
pub fn build_consumer_release_report() -> [u8; 5] {
    [2, 0, 0, 0, 0]
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
    // Consumer report construction
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_consumer_report_play_pause() {
        // Play/Pause is Consumer Page usage 0xCD.
        let report = build_consumer_report(0xCD).unwrap();
        assert_eq!(report[0], 2); // Report ID
        assert_eq!(report[1], 0xCD); // Press field, low byte
        assert_eq!(report[2], 0x00); // Press field, high byte
        assert_eq!(&report[3..], &[0u8; 2]); // Release field zero
    }

    #[test]
    fn test_build_consumer_report_high_usage() {
        // Volume Up is Consumer Page usage 0xE9.
        let report = build_consumer_report(0xE9).unwrap();
        assert_eq!(report[1], 0xE9);
        assert_eq!(report[2], 0x00);
    }

    #[test]
    fn test_build_consumer_report_unknown_usage() {
        // Usage 0x00 is not a known consumer control.
        assert!(matches!(
            build_consumer_report(0x00),
            Err(HidVirtKbdConnError::UnknownConsumerUsage(0x00))
        ));
    }

    #[test]
    fn test_build_consumer_release_report() {
        // An all-clear report: report ID 2, both fields zero.
        assert_eq!(build_consumer_release_report(), [2, 0, 0, 0, 0]);
    }

    /// Simulate the report sequence for a consumer key press and release.
    #[test]
    fn test_consumer_press_release_sequence() {
        // Press Play/Pause (0xCD).
        let press = build_consumer_report(0xCD).unwrap();
        assert_eq!(press, [2, 0xCD, 0x00, 0x00, 0x00]);

        // Release: all-clear report.
        let release = build_consumer_release_report();
        assert_eq!(release, [2, 0x00, 0x00, 0x00, 0x00]);
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
