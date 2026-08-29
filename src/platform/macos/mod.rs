// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

mod active_app;
mod iokit_hid;
mod karabiner_client;
mod keyboard;
mod keycode;
mod mapping;

pub(crate) use active_app::get_active_app_name;
pub use iokit_hid::{
    HidDevice, HidDeviceManager, HidQueue, HidQueueHandle, HidValueCallback,
    IOHIDQueue, for_each_hid_value,
};
pub use karabiner_client::{
    INJECTION_KEYBOARD_IDENTITY, KarabinerClient, KeyboardIdentity,
    OUTPUT_KEYBOARD_IDENTITY,
};
pub use keyboard::list_keyboards;
pub use keycode::{cg_keycode_to_hid_usage, cg_keycode_to_hid_usage_full};
pub use mapping::start_mapping;
