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

mod capture;
mod device;
mod ffi;

// Some of these items are only referenced within this module tree, but they
// were part of the module's public surface before the split, so they are
// re-exported unchanged.
#[allow(unused_imports)]
pub use capture::{
    HidQueueContext, SeizureHandle, for_each_hid_value,
    start_iohid_seizure_mapping,
};
pub use device::{
    HidDevice, HidDeviceManager, HidQueue, HidQueueHandle, HidValueCallback,
};
#[allow(unused_imports)]
pub use ffi::{
    IOHIDDevice, IOHIDElement, IOHIDManager, IOHIDQueue, IOHIDValue,
    IoKitError,
};
