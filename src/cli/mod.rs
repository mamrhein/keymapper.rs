// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Implementations of the `keymapper` CLI commands, platform-specific where
//! the operation is (service management, keyboard discovery, key probing)
//! and portable otherwise (`config`, `appnames`).
//!
//! This module holds the command *bodies*; the `keymapper` binary keeps only
//! argument parsing and dispatch.  It is `pub` solely because the binary is a
//! separate crate in the same package; it is not part of the supported
//! external API.

pub mod appnames_cmd;
pub mod config_cmd;
pub mod daemon_cmd;
pub mod keyboard_cmd;
pub mod keys_cmd;
