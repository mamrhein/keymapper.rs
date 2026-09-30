// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Configuration parsing and hardened reading, path resolution, keyboard
//! discovery types, modifier definitions, and the HID-centric key identity
//! ([`HidUsage`]) used as the canonical key type across all platforms.
//!
//! Application-identity queries live in
//! [`platform::app_identity`](crate::platform::app_identity), alongside the
//! rest of the OS-specific code.

pub mod config;
pub mod config_io;
pub mod config_path;
pub(crate) mod frame;
pub mod hid_usage;
pub mod keyboard;
pub(crate) mod modifier;
pub(crate) mod paths;

pub use hid_usage::HidUsage;
pub use keyboard::{
    KeyboardInfo, KeyboardSpecifier, filter_keyboards_by_specifiers,
};
