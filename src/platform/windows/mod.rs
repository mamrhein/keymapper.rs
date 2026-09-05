// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

mod config_dir;
mod dispatch;
mod key;
mod keyboard;
mod mapping;
pub(crate) mod raw_input;

/// Magic `dwExtraInfo` tag stamped on every key the daemon injects through
/// `SendInput` — mapped outputs in normal mode and re-emissions in capture
/// mode (`KEYMAPPER_CAPTURE`).
///
/// The daemon's hook proc matches on this tag to pass its own injections
/// through without re-mapping them, and the e2e monitor's `WH_KEYBOARD_LL`
/// hook filters on it to capture exactly the daemon's output — no window or
/// keyboard focus required.  A distinctive value keeps it from colliding
/// with tags other input sources may use.
pub const INJECTED_TAG: usize = 0x4B_4D_50_01;

pub use config_dir::config_dir;
pub use key::{Key, keycode_to_hid_usage};
pub use keyboard::list_keyboards;
pub use mapping::start_mapping;
