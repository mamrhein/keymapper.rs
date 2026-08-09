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
/// Defines an input report with modifier byte, reserved byte, and six key-code
/// slots, plus an LED output report and a Consumer Control page for media keys.
///
/// Report layout (input, report ID 1):
///   Byte 1: Report ID       (always 1)
///   Byte 2: Modifier bitmask
///   Byte 3: Reserved
///   Bytes 4--9: Key codes   (up to 6 simultaneously pressed keys)
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
    // ── Input Report ──────────────────────────────────────────────
    0x05, 0x01,                           // USAGE_PAGE (Generic Desktop)
    0x09, 0x06,                           // USAGE (Keyboard)
    0xa1, 0x01,                           // COLLECTION (Application)

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

    // ── LED Output Report (report ID 2) ───────────────────────────
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

    // ── Consumer Control Page (report ID 3) ──────────────────────
    0x85, 0x03,                           //   REPORT_ID (3)
    0x05, 0x0c,                           //   USAGE_PAGE (Consumer)
    0x19, 0xb5,                           //   USAGE_MINIMUM (Play/Pause)
    0x29, 0xb7,                           //   USAGE_MAXIMUM (Stop)
    0x15, 0x00,                           //   LOGICAL_MINIMUM (0)
    0x25, 0x01,                           //   LOGICAL_MAXIMUM (1)
    0x75, 0x01,                           //   REPORT_SIZE (1)
    0x95, 0x03,                           //   REPORT_COUNT (3)
    0x81, 0x02,                           //   INPUT (Data, Var, Abs)

    0x95, 0x01,                           //   REPORT_COUNT (1)
    0x75, 0x05,                           //   REPORT_SIZE (5 padding bits)
    0x81, 0x03,                           //   INPUT (Cnst, Var, Abs)

    0xc0                                  // END_COLLECTION
};
