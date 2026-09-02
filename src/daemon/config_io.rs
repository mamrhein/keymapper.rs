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
//! Both the initial load at daemon startup and the hot-reload path must read
//! the same file under identical security constraints.  This module provides
//! a single [`read_config_content`] helper that both call, so the two paths
//! can never drift apart.  On Unix the file is opened with `O_NOFOLLOW` and
//! every check (regular file, size, ownership, world-writable) is performed on
//! that same descriptor, eliminating TOCTOU races between metadata inspection
//! and the content read.

use std::{io::Read, path::Path};

use thiserror::Error;

/// Maximum config file size in bytes (1 MB).  A key-mapping configuration
/// should never approach this limit; a larger file indicates either a write
/// gone wrong or an adversarial payload.
pub(crate) const MAX_CONFIG_SIZE: u64 = 1024 * 1024;

/// Error returned when the config file cannot be read safely.
#[derive(Debug, Error)]
pub enum ConfigReadError {
    /// The config file does not exist or could not be opened.
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

    /// The config file is owned by a different user.
    #[cfg(unix)]
    #[error("config file is owned by uid {uid} (current user: {current})")]
    WrongOwner { uid: u32, current: u32 },

    /// The config file is world-writable.
    #[cfg(unix)]
    #[error("config file is world-writable")]
    WorldWritable,

    /// The config content could not be read.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Open the config file with hardening and read its full content.
///
/// The checks, in order: the path is not a symlink; the file opens without
/// following symlinks (`O_NOFOLLOW` on Unix); it is a regular file; its size
/// is within [`MAX_CONFIG_SIZE`]; on Unix it is owned by the current user
/// (unless running as root) and is not world-writable.  All checks run on the
/// single open descriptor, so there is no window in which the file can be
/// swapped between inspection and read.
pub(crate) fn read_config_content(
    path: &Path,
) -> Result<String, ConfigReadError> {
    // Security check: verify the file is not a symlink.  This is an extra
    // guard beyond O_NOFOLLOW below, covering edge cases such as parent
    // directory components being replaced with symlinks.
    let sym_meta = std::fs::symlink_metadata(path)
        .map_err(|_| ConfigReadError::NotFound)?;
    if sym_meta.file_type().is_symlink() {
        return Err(ConfigReadError::Symlink);
    }

    // Open the file.  On Unix we use O_NOFOLLOW so a symlink planted between
    // the check above and the open is never followed, and we can then do the
    // metadata checks and read on the same descriptor.
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;

        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| ConfigReadError::NotFound)?
    };

    #[cfg(not(unix))]
    let mut file =
        std::fs::File::open(path).map_err(|_| ConfigReadError::NotFound)?;

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

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        // Security check: file is owned by the current user.  Skipped when
        // running as root: a root daemon legitimately reads configs owned by
        // regular users (the production layout keeps the config in the user's
        // home directory), and the world-writable check below is the
        // meaningful tamper guard in that case.
        let current_uid = unsafe { libc::getuid() };
        let uid = metadata.uid();
        if current_uid != 0 && uid != current_uid {
            return Err(ConfigReadError::WrongOwner {
                uid,
                current: current_uid,
            });
        }

        // Security check: file is not world-writable (prevents other users on
        // the same system from tampering with it).
        let mode = metadata.mode() as libc::mode_t;
        if (mode & libc::S_IWOTH) != 0 {
            return Err(ConfigReadError::WorldWritable);
        }
    }

    // Read content from the already-open handle — no race with metadata.
    let mut content = String::new();
    file.read_to_string(&mut content)?;
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

    #[cfg(unix)]
    #[test]
    fn symlink_is_rejected() {
        let target = write_temp("symlink_target", "groups: []");
        let link = target.with_file_name(format!(
            "{}.link",
            target.file_name().unwrap().to_string_lossy()
        ));
        std::os::unix::fs::symlink(&target, &link)
            .expect("failed to create symlink");

        let err = read_config_content(&link).unwrap_err();
        std::fs::remove_file(&link).ok();
        std::fs::remove_file(&target).ok();

        assert!(matches!(err, ConfigReadError::Symlink));
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_is_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let path = write_temp("world_writable", "groups: []");
        std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(0o666),
        )
        .expect("failed to chmod");

        let err = read_config_content(&path).unwrap_err();
        std::fs::remove_file(&path).ok();

        assert!(matches!(err, ConfigReadError::WorldWritable));
    }
}
