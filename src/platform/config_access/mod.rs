// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The OS-specific hardened access behind the config-file reader.
//!
//! [`crate::common::config_io`] owns the policy of the hardened config read
//! (symlink refusal, regular-file and size limits) and the order in which
//! the checks run; this facet owns the *OS-specific enforcement*: the
//! symlink-safe open and, on Unix, the verification of the
//! parent-directory chain and of the file's ownership and mode.  Splitting
//! the two keeps every `#[cfg(unix)]` byte of the hardened read under
//! `platform/` (one home for platform code) while the shared reading logic
//! stays in `common`.
//!
//! The facet exposes two free functions, selected per platform below.
//! Their shared contract:
//!
//! - `open_config_file(path)`: open *path* for reading with the platform's
//!   hardening.  Errors map as the shared reader expects:
//!   [`NotFound`](crate::common::config_io::ConfigReadError::NotFound) for a
//!   missing file,
//!   [`Symlink`](crate::common::config_io::ConfigReadError::Symlink) for a
//!   symlink in the final position, and
//!   [`Io`](crate::common::config_io::ConfigReadError::Io) with the original
//!   error preserved otherwise.
//! - `check_file_trust(metadata)`: validate the metadata of the already-open
//!   file against the platform's trust rules and return `Ok` when the file may
//!   be read.
//!
//! The dependency arrows run `platform -> common`: the facet consumes
//! [`ConfigReadError`](crate::common::config_io::ConfigReadError) from
//! `common::config_io` and never names the reader itself, so `common`
//! orchestrates a contract it does not implement.  `common::config_io`
//! reaches the two functions below through the same sanctioned
//! `common -> platform` seam as `config_dir` and `console_user_home`.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// Validate the metadata of the already-open config file.
#[cfg(unix)]
pub(crate) use unix::check_file_trust;
/// Open the config file with the platform's hardening guarantees.
#[cfg(unix)]
pub(crate) use unix::open_config_file;
/// Validate the metadata of the already-open config file.
#[cfg(windows)]
pub(crate) use windows::check_file_trust;
/// Open the config file with the platform's hardening guarantees.
#[cfg(windows)]
pub(crate) use windows::open_config_file;
