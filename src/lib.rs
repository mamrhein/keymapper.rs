// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Cross-platform key-remapping daemon and CLI for macOS, Linux, and Windows.
//!
//! # Public surface
//!
//! The crate is a library only so its three binaries (`keymapper`,
//! `keymapperd`, `virtkbdd`) and the `tests/` and `test-util/` crates can
//! share code.  The intended public surface — what those consumers actually
//! use — is deliberately narrow:
//!
//! - [`common`] — configuration types ([`common::config`]), the hardened
//!   config reader ([`common::config_io`]), path resolution
//!   ([`common::config_path`]), keyboard discovery types
//!   ([`common::keyboard`]), and the canonical key identity ([`HidUsage`],
//!   also re-exported at the crate root).
//! - [`daemon`] — the runtime control socket ([`daemon::control`]), the log
//!   backend ([`daemon::logging`]), and the live state ([`daemon::state`]) and
//!   config-watcher ([`daemon::watcher`]) used by the `keymapperd` binary and
//!   its integration tests.
//! - [`keymap_core`] — the compiled mapping cache
//!   ([`keymap_core::mapping_cache`]), the
//!   [`Lookup`](keymap_core::lookup::Lookup) abstraction, and the shared
//!   debug-log grammar ([`keymap_core::logfmt`]) that the e2e harness parses.
//! - [`platform`] — the documented per-OS backend surface (input capture and
//!   injection, `list_keyboards`, `start_mapping`, `config_dir`,
//!   `keycode_to_hid_usage`, and the `app_identity` facet).
//! - [`cli`] — the `keymapper` command implementations.  This is public only
//!   because the `keymapper` binary is a separate crate in the same package;
//!   it is *not* a supported API and may change without notice.
//!
//! Everything else is implementation detail: internal decision/emission
//! modules, the mapping engine, and platform internals are `pub(crate)` or
//! module-private.  Layering rules (`platform -> keymap_core -> common`, with
//! `daemon` on top) are documented on each module and exercised by
//! `tests/api_surface.rs`, which imports only through this facade.

pub mod cli;
pub mod common;
pub mod daemon;
pub mod keymap_core;
pub mod platform;

// Re-export the HID-centric key identity so downstream code (and tests)
// can refer to it via the crate root.
pub use common::HidUsage;
