// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

pub mod hid_translate;
mod keyboard;
mod mapping;

pub use keyboard::{discover_and_open_keyboards, list_keyboards};
pub(crate) use mapping::VIRTUAL_KEYBOARD_NAME;
pub use mapping::start_mapping;
