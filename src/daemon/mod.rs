// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Daemon runtime: the unified mapping engine, mapping cache, state
//! management, and config hot-reload.

pub mod config_io;
pub mod engine;
pub mod mapping_cache;
pub mod state;
#[cfg(feature = "e2e")]
pub mod test_hooks;
#[cfg(test)]
pub(crate) mod test_lookup;
pub mod watcher;
