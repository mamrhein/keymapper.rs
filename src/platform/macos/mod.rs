// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

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

pub use config_dir::{config_dir, console_user_home};
// `KarabinerClient` is a test-harness export only (the injector opens its
// own Karabiner connection); gate it behind the `test-util` feature like
// the rest of the harness surface.  The production emit path reaches the
// client through `karabiner_client` directly, not this re-export.
#[cfg(feature = "test-util")]
pub use karabiner_client::KarabinerClient;
pub use karabiner_client::{
    INJECTION_KEYBOARD_IDENTITY, KeyboardIdentity, OUTPUT_KEYBOARD_IDENTITY,
};
pub use keyboard::list_keyboards;
pub use keycode::keycode_to_hid_usage;
pub use mapping::start_mapping;
pub use virtkbd::start_virtkbd;
