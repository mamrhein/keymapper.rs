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
pub mod key;
pub mod keyboard;
pub(crate) mod modifier;

pub use key::Key;
pub(crate) use key::unknown_key_error;
pub use keyboard::{KeyboardInfo, KeyboardSpecifier};
