// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

/// @file
/// Wire protocol shared between the DriverKit virtual HID keyboard driver and
/// the user-space client.
///
/// The user-space client opens the driver with `IOServiceOpen()` and sends HID
/// reports through `IOConnectCallMethod()`. Each call is identified by a
/// selector, and the report payload is passed as the call's structure input.
///
/// This header is the single source of truth for the selector values. The
/// Rust client mirrors these constants (see `src/platform/macos/hid_socket.rs`),
/// so any change here must be reflected there.

#pragma once

/// Selector for sending an HID input report to the driver.
///
/// The report bytes are passed as the structure input of the
/// `IOConnectCallMethod()` call. The driver feeds the report into the HID
/// event system, emulating a real hardware keyboard.
#define kKeyMapperSendReportSelector 0
