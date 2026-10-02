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
mod emit;
mod ipc_client;
mod ipc_frame;
mod ipc_server;
mod karabiner_client;
mod keyboard;
mod keycode;
mod mapping;
mod virtkbd;

// `KarabinerClient` is a test-harness export only (the injector opens its
// own Karabiner connection); gate it behind the `test-util` feature like
// the rest of the harness surface.  The production emit path reaches the
// client through `karabiner_client` directly, not this re-export.
pub(crate) use backend::MacOsBackend;
// `list_keyboards` and `start_mapping` are the [`crate::platform`]
// exports; on macOS they are thin shims that drive the
// [`crate::platform::backend`] contract (see the `backend` submodule).
pub use backend::{list_keyboards, start_mapping};
pub use config_dir::{config_dir, console_user_home};
#[cfg(feature = "test-util")]
pub use karabiner_client::{INJECTION_KEYBOARD_IDENTITY, KarabinerClient};
pub use keycode::keycode_to_hid_usage;
pub use virtkbd::start_virtkbd;
