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
mod device_match;
mod key;
mod keyboard;
mod mapping;
pub(crate) mod raw_input;
mod raw_worker;

/// Magic `dwExtraInfo` tag stamped on every key the daemon injects through
/// `SendInput`.
///
/// The daemon's hook proc matches on this tag to pass its own injections
/// through without re-mapping them.  A distinctive value keeps it from
/// colliding with tags other input sources may use.
///
/// `pub(crate)` because it is a self-echo-suppression detail of the Windows
/// capture path (read by `windows::mapping`); it was never a supported
/// cross-platform export, only a dead public re-export (F5b).
pub(crate) const INJECTED_TAG: usize = 0x4B_4D_50_01;

// `list_keyboards` and `start_mapping` are the [`crate::platform`]
// exports; on Windows they are thin shims that drive the
// [`crate::platform::backend`] contract (see the `backend` submodule).
pub(crate) use backend::WindowsBackend;
pub use backend::{list_keyboards, start_mapping};
pub use config_dir::config_dir;
// `Key` is part of the public surface only for the test harness (its
// `from_hid_usage`/`as_native` drive the Windows injector); gate it
// behind the same `test-util` feature as the rest of the harness surface.
#[cfg(feature = "test-util")]
pub use key::Key;
pub use key::keycode_to_hid_usage;
