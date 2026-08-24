// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Platform-specific implementations of CLI commands.

pub mod appnames_cmd;
pub mod daemon_cmd;
pub mod keyboard_cmd;
pub mod keys_cmd;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod server_cmd;
