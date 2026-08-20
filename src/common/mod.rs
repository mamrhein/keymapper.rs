// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Shared configuration parsing, path resolution, modifier definitions, and
//! platform-agnostic key identities.

pub mod config;
pub mod config_path;
pub mod hid_usage;
pub mod key;
pub mod keyboard;
pub(crate) mod modifier;

pub use hid_usage::HidUsage;
pub use key::Key;
pub use keyboard::{
    KeyboardInfo, KeyboardSpecifier, filter_keyboards_by_specifiers,
};
