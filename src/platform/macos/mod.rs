// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

mod active_app;
#[cfg(feature = "driverkit")]
mod hid_socket;
mod ioh_device;
mod iokit_hid;
mod key;
mod keyboard;
mod mapping;

pub(crate) use active_app::get_active_app_name;
#[cfg(feature = "driverkit")]
pub use hid_socket::HidSocket;
pub use ioh_device::iohid_available;
pub use key::Key;
pub use keyboard::list_keyboards;
pub use mapping::start_mapping;
