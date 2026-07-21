// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

pub mod common;
pub mod mapping_cache;
pub mod platform;
pub mod state;
pub mod watcher;

// Re-export the platform-specific Key type so downstream code (and tests)
// can refer to it via the crate root.
pub use platform::Key;
