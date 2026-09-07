// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! IOKit HID device access for the e2e keyboard monitor on macOS.
//!
//! The daemon no longer captures input through IOKit (keymapperd uses a
//! CGEventTap in the user domain); this module remains because the e2e
//! monitor seizes the daemon's Karabiner DriverKit virtual keyboard and logs
//! the raw HID events it emits.  Seizing a device requires root privileges
//! and the Input Monitoring permission in System Settings.
#![allow(dead_code, non_snake_case)]

mod device;
mod ffi;

pub use device::{
    HidDevice, HidDeviceManager, HidQueue, HidQueueHandle, HidValueCallback,
    for_each_hid_value,
};
// Some of these items are only referenced within this module tree, but
// they were part of the module's public surface before the split, so they
// are re-exported unchanged.
#[allow(unused_imports)]
pub use ffi::{
    IOHIDDevice, IOHIDElement, IOHIDManager, IOHIDQueue, IOHIDValue,
    IoKitError,
};
