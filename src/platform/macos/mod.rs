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
mod mapping;

pub(crate) use active_app::get_active_app_name;
pub use iokit_hid::{
    HidDevice, HidDeviceManager, HidQueue, HidQueueHandle, HidValueCallback,
    IOHIDQueue, for_each_hid_value,
};
pub use karabiner_client::KarabinerClient;
pub use keyboard::list_keyboards;
pub use mapping::start_mapping;
