// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Daemon runtime: the unified mapping engine, mapping cache, state
//! management, config hot-reload, and the control socket for runtime
//! configuration of a running daemon.

pub mod config_io;
pub mod control;
pub mod engine;
pub mod logging;
pub mod mapping_cache;
pub mod state;
#[cfg(test)]
pub(crate) mod test_lookup;
pub mod watcher;
