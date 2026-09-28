// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Daemon runtime: live state management, config hot-reload, and the
//! control socket for runtime configuration of a running daemon.
//!
//! The cross-platform mapping engine and the compiled mapping cache are
//! homed in [`crate::keymap_core`]; [`state::RuntimeState`] composes them
//! with keyboard discovery and the active-app query into the runtime the
//! platform backends drive.
//!
//! Layering: `daemon -> keymap_core -> common`, and `daemon -> platform`
//! (which itself sits on `keymap_core`).  The daemon depends on the core
//! and the platform layer, never the other way around.

pub mod control;
pub mod logging;
pub mod state;
pub mod watcher;
