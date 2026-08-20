// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

/// @file
/// Standard USB HID keyboard report descriptor.
///
/// Defines two top-level application collections:
///
/// 1. A Keyboard collection with an input report (modifier byte, reserved
///    byte, and six key-code slots) and an LED output report.
/// 2. A Consumer Control collection with an input report and a matching
///    output report for media/display keys.
///
/// Report layout (keyboard input, report ID 1):
///   Byte 1: Report ID       (always 1)
///   Byte 2: Modifier bitmask
///   Byte 3: Reserved
///   Bytes 4--9: Key codes   (up to 6 simultaneously pressed keys)
///
/// Report layout (consumer input, report ID 2):
///   Byte 1: Report ID        (always 2)
///   Bytes 2--3: Press usage   (16-bit Consumer Page usage)
///   Bytes 4--5: Release usage (16-bit Consumer Page usage)
///
/// A consumer key is pressed by placing its usage in the press field and
/// released by clearing both fields to zero. macOS interprets these reports
/// as native media key events.
///
/// Modifier bit positions:
///   0 -- Left Control
///   1 -- Left Shift
///   2 -- Left Option (Alt)
///   3 -- Left Command
///   4 -- Right Control
///   5 -- Right Shift
///   6 -- Right Option (Alt)
///   7 -- Right Command

#pragma once

#include <stdint.h>

/// Standard USB HID keyboard report descriptor.
constexpr uint8_t kKeyboardReportDescriptor[] = {
    // ── Keyboard Collection ───────────────────────────────────────
    0x05, 0x01,                           // USAGE_PAGE (Generic Desktop)
    0x09, 0x06,                           // USAGE (Keyboard)
    0xa1, 0x01,                           // COLLECTION (Application)

    // Keyboard Input Report (report ID 1)
    0x85, 0x01,                           //   REPORT_ID (1)

    // Modifier byte (8 bits × 1 report = 1 byte)
    0x05, 0x07,                           //   USAGE_PAGE (Keyboard/Keypad)
    0x19, 0xe0,                           //   USAGE_MINIMUM (LeftControl)
    0x29, 0xe7,                           //   USAGE_MAXIMUM (RightGUI)
    0x15, 0x00,                           //   LOGICAL_MINIMUM (0)
    0x25, 0x01,                           //   LOGICAL_MAXIMUM (1)
    0x75, 0x01,                           //   REPORT_SIZE (1)
    0x95, 0x08,                           //   REPORT_COUNT (8)
    0x81, 0x02,                           //   INPUT (Data, Var, Abs)

    // Reserved byte
    0x95, 0x01,                           //   REPORT_COUNT (1)
    0x75, 0x08,                           //   REPORT_SIZE (8)
    0x81, 0x03,                           //   INPUT (Cnst, Var, Abs)

    // Key codes (6 bytes × 8 bits, range 0--101)
    0x95, 0x06,                           //   REPORT_COUNT (6)
    0x75, 0x08,                           //   REPORT_SIZE (8)
    0x15, 0x00,                           //   LOGICAL_MINIMUM (0)
    0x25, 0x65,                           //   LOGICAL_MAXIMUM (101)
    0x05, 0x07,                           //   USAGE_PAGE (Keyboard/Keypad)
    0x19, 0x00,                           //   USAGE_MINIMUM (Reserved)
    0x29, 0x65,                           //   USAGE_MAXIMUM (101)
    0x81, 0x00,                           //   INPUT (Data, Array)

    // LED Output Report (no report ID)
    0x95, 0x05,                           //   REPORT_COUNT (5 LEDs)
    0x75, 0x01,                           //   REPORT_SIZE (1 bit each)
    0x15, 0x00,                           //   LOGICAL_MINIMUM (0)
    0x25, 0x01,                           //   LOGICAL_MAXIMUM (1)
    0x05, 0x08,                           //   USAGE_PAGE (LEDs)
    0x19, 0x01,                           //   USAGE_MINIMUM (Num Lock)
    0x29, 0x05,                           //   USAGE_MAXIMUM (Kana)
    0x91, 0x02,                           //   OUTPUT (Data, Var, Abs)

    0x95, 0x01,                           //   REPORT_COUNT (1)
    0x75, 0x03,                           //   REPORT_SIZE (3 padding bits)
    0x91, 0x03,                           //   OUTPUT (Cnst, Var, Abs)

    0xc0,                                 // END_COLLECTION

    // ── Consumer Control Collection ───────────────────────────────
    0x05, 0x0c,                           // USAGE_PAGE (Consumer)
    0x09, 0x01,                           // USAGE (Consumer Control)
    0xa1, 0x01,                           // COLLECTION (Application)

    // Consumer Input Report (report ID 2)
    0x85, 0x02,                           //   REPORT_ID (2)
    0x15, 0x00,                           //   LOGICAL_MINIMUM (0)
    0x26, 0xff, 0x00,                     //   LOGICAL_MAXIMUM (255)
    0x75, 0x10,                           //   REPORT_SIZE (16 bits)
    0x95, 0x02,                           //   REPORT_COUNT (2 — press and release fields)
    0x19, 0x00,                           //   USAGE_MINIMUM (0)
    0x2a, 0xff, 0x00,                     //   USAGE_MAXIMUM (255)
    0x81, 0x00,                           //   INPUT (Data, Var, Abs)

    // Matching Consumer Output Report (report ID 2) for clearing consumer
    // state. Reuses the field definitions above (two 16-bit usage fields).
    0x91, 0x00,                           //   OUTPUT (Data, Var, Abs)

    0xc0                                  // END_COLLECTION
};
