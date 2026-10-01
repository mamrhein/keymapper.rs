// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

mod backend;
mod capture;
mod config_dir;
pub mod hid_translate;
mod keyboard;
mod mapping;

pub(crate) use backend::LinuxBackend;
pub use backend::{list_keyboards, start_mapping};
pub use config_dir::config_dir;
pub use hid_translate::keycode_to_hid_usage;
pub use mapping::VIRTUAL_KEYBOARD_NAME;
