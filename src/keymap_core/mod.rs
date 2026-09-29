// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The cross-platform key-mapping core: the platform-agnostic decision
//! engine (`engine`), the shared `Decision` interpretation (`emission`),
//! the unified debug-log grammar ([`logfmt`]), the compiled mapping cache
//! ([`mapping_cache`]), and the [`Lookup`](lookup::Lookup) abstraction the
//! platform backends consume to resolve a pressed key to its outputs.
//!
//! The `engine` and `emission` modules are internal to the crate
//! (`pub(crate)`): they drive the platform backends' capture path but are
//! not part of the public surface (the log grammar, the compiled cache, and
//! the [`Lookup`](lookup::Lookup) abstraction are).
//!
//! This is the shared bottom layer of the mapping stack.  Every platform
//! backend (Linux, macOS, Windows) drives its capture path through the
//! types here, and the daemon composes them into a live runtime
//! (`daemon::state::RuntimeState` implements [`Lookup`](lookup::Lookup)).
//! Nothing in this module is daemon-specific: it depends only on
//! [`common`](crate::common), never on [`daemon`](crate::daemon) or
//! [`platform`](crate::platform).
//!
//! Layering rule: `platform -> keymap_core -> common`.  The platform layer
//! may depend on this module and on `common`; `common` must not depend on
//! either of the two above it.

pub(crate) mod emission;
pub(crate) mod engine;
pub mod logfmt;
pub mod lookup;
pub mod mapping_cache;
#[cfg(test)]
pub(crate) mod test_lookup;
