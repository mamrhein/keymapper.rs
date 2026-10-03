// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Hardened reading of the configuration file.
//!
//! The daemon's initial load at startup, its hot-reload path, and the CLI
//! `config list/check/add` subcommands all read the same file under
//! identical security constraints.  This module provides a single
//! [`read_config_content`] helper that every path calls, so they can never
//! drift apart.  The file is opened without following symlinks and every
//! check (regular file, size, ownership, world-writable) is performed on
//! that same descriptor, eliminating TOCTOU races between metadata
//! inspection and the content read.  Before the content is returned, the
//! parent-directory chain is verified as well (SEC-19): an ancestor that
//! cannot be inspected, is not a directory, or is world-writable without
//! the sticky bit aborts the read.  Components are inspected with symlinks
//! resolved, so trust attaches to the directory that is actually
//! traversed, never to the link or name used to reach it.
//!
//! This module owns the shared sequence and the platform-neutral checks;
//! the OS-specific enforcement (on Unix the `O_NOFOLLOW` open, the
//! parent-directory-chain inspection, and the ownership and mode checks;
//! elsewhere a plain open with no mode checks) lives in
//! [`crate::platform::config_access`].

use std::{io::Read, path::Path};

use log::info;
use thiserror::Error;

use crate::platform::config_access;

/// Maximum config file size in bytes (1 MB).  A key-mapping configuration
/// should never approach this limit; a larger file indicates either a write
/// gone wrong or an adversarial payload.
pub const MAX_CONFIG_SIZE: u64 = 1024 * 1024;

/// Error returned when the config file cannot be read safely.
#[derive(Debug, Error)]
pub enum ConfigReadError {
    /// The config file does not exist.
    #[error("config file not found")]
    NotFound,

    /// The config path is a symlink.
    #[error("config file is a symlink")]
    Symlink,

    /// The metadata of the open file could not be read.
    #[error("failed to read config file metadata")]
    Metadata,

    /// The config path is not a regular file.
    #[error("config path is not a regular file")]
    NotRegularFile,

    /// The config file exceeds [`MAX_CONFIG_SIZE`].
    #[error("config file is too large ({size} bytes, limit {limit})")]
    TooLarge { size: u64, limit: u64 },

    /// The config file is owned by a different user (constructed on Unix
    /// only).
    #[error("config file is owned by uid {uid} (current user: {current})")]
    WrongOwner { uid: u32, current: u32 },

    /// The config file is world-writable (constructed on Unix only).
    #[error("config file is world-writable")]
    WorldWritable,

    /// A parent directory of the config path could not be trusted (Unix
    /// only): a component that cannot be inspected, is not a directory,
    /// or is world-writable without the sticky bit may let a user who
    /// does not own the config redirect the path to a file they control.
    #[error("untrusted parent directory '{path}' ({reason})")]
    UntrustedParentDir { path: String, reason: &'static str },

    /// The config file could not be opened or read for a reason other
    /// than absence (e.g. a permission or symlink-chain failure).  The
    /// distinct I/O error kind is preserved for diagnostics.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Open the config file with hardening and read its full content.
///
/// Shared by the daemon (initial load and hot-reload) and the CLI so both
/// sides enforce identical constraints on the same trust boundary.
///
/// The checks, in order: the path is not a symlink; the file opens through
/// [`crate::platform::config_access`], which on Unix first verifies the
/// parent-directory chain (every component must resolve to a directory
/// that is not world-writable; sticky-bit protected shared directories
/// such as `/tmp` are exempted because custom CLI config paths under the
/// system temp directory are legitimate and the daemon's config search
/// path never lies inside one) and then opens without following symlinks
/// (`O_NOFOLLOW`); the file is a regular file; its size is within
/// [`MAX_CONFIG_SIZE`]; the platform trust check accepts it (on Unix: it
/// is owned by the current user unless running as root, and it is not
/// world-writable).  All checks run on the single open descriptor, so
/// there is no window in which the file can be swapped between inspection
/// and read.  Because the chain is re-inspected on every call, a symlink
/// swapped into a parent directory after startup aborts the next read
/// instead of being followed silently.
pub fn read_config_content(path: &Path) -> Result<String, ConfigReadError> {
    // Security check: verify the config file itself is not a symlink.
    // This is an extra guard beyond the platform's O_NOFOLLOW (Unix),
    // closing the window in which a symlink could be planted between
    // inspection and open.  Only ENOENT means "not found"; other failures
    // (e.g. EACCES from an unsearchable parent directory) keep their
    // distinct error kind.
    let sym_meta = std::fs::symlink_metadata(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ConfigReadError::NotFound
        } else {
            ConfigReadError::Io(err)
        }
    })?;
    if sym_meta.file_type().is_symlink() {
        return Err(ConfigReadError::Symlink);
    }

    // Open with the platform's hardening: on Unix this verifies the
    // parent-directory chain (SEC-19) first, then opens with O_NOFOLLOW so
    // a symlink planted between the check above and the open is refused
    // rather than followed.
    let mut file = config_access::open_config_file(path)?;

    let metadata = file.metadata().map_err(|_| ConfigReadError::Metadata)?;

    if !metadata.is_file() {
        return Err(ConfigReadError::NotRegularFile);
    }

    // Security check: file size is within acceptable bounds.
    if metadata.len() > MAX_CONFIG_SIZE {
        return Err(ConfigReadError::TooLarge {
            size: metadata.len(),
            limit: MAX_CONFIG_SIZE,
        });
    }

    // Platform-specific trust checks, run on the open descriptor's
    // metadata (Unix: ownership and world-writable mode).
    config_access::check_file_trust(&metadata)?;

    // Read content from the already-open handle — no race with metadata.
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    info!("Read config from {}", path.display());
    Ok(content)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Write *content* to a temp file and return its path.  Each invocation
    /// gets a unique filename keyed by *label* to avoid races when tests run
    /// in parallel.  The caller must delete the file (or let the process
    /// exit).
    fn write_temp(label: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("keymapperd_config_io_{}.yaml", label));
        std::fs::write(&path, content).expect("failed to write temp config");
        path
    }

    #[test]
    fn reads_valid_file() {
        let path = write_temp("valid", "groups: []");
        let content = read_config_content(&path).expect("should read");
        std::fs::remove_file(&path).ok();

        assert_eq!(content, "groups: []");
    }

    #[test]
    fn missing_file_is_not_found() {
        let err = read_config_content(std::path::Path::new(
            "/nonexistent/path/config.yaml",
        ))
        .unwrap_err();

        assert!(matches!(err, ConfigReadError::NotFound));
    }

    #[test]
    fn oversized_file_is_rejected() {
        // One byte over the limit.
        let big = "a".repeat(MAX_CONFIG_SIZE as usize + 1);
        let path = write_temp("oversized", &big);
        let err = read_config_content(&path).unwrap_err();
        std::fs::remove_file(&path).ok();

        assert!(matches!(err, ConfigReadError::TooLarge { .. }));
    }
}
