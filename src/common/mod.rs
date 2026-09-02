// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Shared application identity queries, configuration parsing, path
//! resolution, modifier definitions, and the HID-centric key identity
//! ([`HidUsage`]) used as the canonical key type across all platforms.

pub mod app_identity;
pub mod config;
pub mod config_path;
pub mod daemon_token;
pub mod hid_usage;
pub mod keyboard;
pub(crate) mod modifier;

pub use hid_usage::HidUsage;
pub use keyboard::{
    KeyboardInfo, KeyboardSpecifier, filter_keyboards_by_specifiers,
};
