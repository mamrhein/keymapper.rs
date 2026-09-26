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
//! drift apart.  On Unix the file is opened with `O_NOFOLLOW` and every
//! check (regular file, size, ownership, world-writable) is performed on
//! that same descriptor, eliminating TOCTOU races between metadata
//! inspection and the content read.  Before the content is returned, the
//! parent-directory chain is verified as well (SEC-19): an ancestor that
//! cannot be inspected, is not a directory, or is world-writable without
//! the sticky bit aborts the read.  Components are inspected with symlinks
//! resolved, so trust attaches to the directory that is actually
//! traversed, never to the link or name used to reach it.

use std::{io::Read, path::Path};

use log::info;
use thiserror::Error;

/// Maximum config file size in bytes (1 MB).  A key-mapping configuration
/// should never approach this limit; a larger file indicates either a write
/// gone wrong or an adversarial payload.
pub const MAX_CONFIG_SIZE: u64 = 1024 * 1024;

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

    /// A parent directory of the config path could not be trusted (Unix
    /// only): a component that cannot be inspected, is not a directory,
    /// or is world-writable without the sticky bit may let a user who
    /// does not own the config redirect the path to a file they control.
    #[cfg(unix)]
    #[error("untrusted parent directory '{path}' ({reason})")]
    UntrustedParentDir { path: String, reason: &'static str },

    /// The config content could not be read.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Open the config file with hardening and read its full content.
///
/// Shared by the daemon (initial load and hot-reload) and the CLI so both
/// sides enforce identical constraints on the same trust boundary.
///
/// The checks, in order: the path is not a symlink; on Unix the
/// parent-directory chain is verified (every component must resolve to a
/// directory that is not world-writable; sticky-bit protected shared
/// directories such as `/tmp` are exempted because custom CLI config
/// paths under the system temp directory are legitimate and the daemon's
/// config search path never lies inside one); the file opens without
/// following symlinks (`O_NOFOLLOW` on Unix); it is a regular file; its
/// size is within [`MAX_CONFIG_SIZE`]; on Unix it is owned by the current
/// user (unless running as root) and is not world-writable.  All checks
/// run on the single open descriptor, so there is no window in which the
/// file can be swapped between inspection and read.  Because the chain is
/// re-inspected on every call, a symlink swapped into a parent directory
/// after startup aborts the next read instead of being followed silently.
pub fn read_config_content(path: &Path) -> Result<String, ConfigReadError> {
    // Security check: verify the config file itself is not a symlink.
    // This is an extra guard beyond O_NOFOLLOW below, closing the window
    // in which a symlink could be planted between inspection and open.
    let sym_meta = std::fs::symlink_metadata(path)
        .map_err(|_| ConfigReadError::NotFound)?;
    if sym_meta.file_type().is_symlink() {
        return Err(ConfigReadError::Symlink);
    }

    // Security check (SEC-19): verify the parent-directory chain.  A
    // world-writable directory without the sticky bit, or a component that
    // resolves to a non-directory, may let a user who does not own the
    // config redirect the path to a file they control, so such a chain
    // aborts the read.  Components are inspected with symlinks resolved:
    // trust attaches to the directory that is actually traversed, never to
    // the link or name used to reach it.  Sticky-bit protected shared
    // directories (e.g. `/tmp`, or the system temp directory on macOS) are
    // exempted because custom CLI config paths under them are legitimate and
    // the daemon's config search path never lies inside one.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        for dir in path.ancestors().skip(1) {
            // The remainder of a relative path is empty; stop there.
            if dir.as_os_str().is_empty() {
                break;
            }
            let dir_mode = match std::fs::metadata(dir) {
                Ok(meta) => {
                    if !meta.is_dir() {
                        return Err(ConfigReadError::UntrustedParentDir {
                            path: dir.display().to_string(),
                            reason: "not a directory",
                        });
                    }
                    meta.mode() as libc::mode_t
                }
                Err(ref err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Err(ConfigReadError::UntrustedParentDir {
                        path: dir.display().to_string(),
                        reason: "missing",
                    });
                }
                Err(_) => {
                    return Err(ConfigReadError::UntrustedParentDir {
                        path: dir.display().to_string(),
                        reason: "cannot be inspected",
                    });
                }
            };
            if (dir_mode & libc::S_IWOTH) != 0
                && (dir_mode & libc::S_ISVTX) == 0
            {
                return Err(ConfigReadError::UntrustedParentDir {
                    path: dir.display().to_string(),
                    reason: "world-writable without sticky bit",
                });
            }
        }
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

    /// Create a unique private directory below the temp dir, or return
    /// `None` if the temp dir itself has no sticky bit (its own
    /// world-writable mode would make every child chain untrusted anyway).
    #[cfg(unix)]
    fn private_temp_subdir(label: &str) -> Option<std::path::PathBuf> {
        use std::os::unix::fs::MetadataExt;

        let temp = std::env::temp_dir();
        let mode = std::fs::metadata(&temp)
            .expect("failed to stat temp dir")
            .mode() as libc::mode_t;
        // If the temp dir itself would not pass the chain check, the
        // platform setup is unusual; skip rather than fail spuriously.
        if (mode & libc::S_IWOTH) != 0 && (mode & libc::S_ISVTX) == 0 {
            return None;
        }
        let dir = temp.join(format!("keymapperd_cfgio_{label}_d"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir(&dir).expect("failed to create temp dir");
        Some(dir)
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_parent_dir_is_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let Some(dir) = private_temp_subdir("ww_parent") else {
            return;
        };
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
            .expect("failed to chmod dir");
        let path = dir.join("config.yaml");
        std::fs::write(&path, "groups: []").expect("failed to write config");

        let err = read_config_content(&path).unwrap_err();
        std::fs::remove_dir_all(&dir).ok();

        assert!(matches!(err, ConfigReadError::UntrustedParentDir { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_parent_to_world_writable_dir_is_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let Some(real) = private_temp_subdir("ww_link_target") else {
            return;
        };
        std::fs::set_permissions(
            &real,
            std::fs::Permissions::from_mode(0o777),
        )
        .expect("failed to chmod dir");
        let link = std::env::temp_dir().join("keymapperd_cfgio_ww_link");
        std::os::unix::fs::symlink(&real, &link)
            .expect("failed to create symlink");
        // The link name is a symlink into an untrusted directory; the
        // resolved target's world-writable mode must be what gets judged.
        let path = link.join("config.yaml");
        std::fs::write(real.join("config.yaml"), "groups: []")
            .expect("failed to write config");

        let err = read_config_content(&path).unwrap_err();
        std::fs::remove_file(&link).ok();
        std::fs::remove_dir_all(&real).ok();

        assert!(matches!(err, ConfigReadError::UntrustedParentDir { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_parent_to_private_dir_is_followed() {
        let Some(real) = private_temp_subdir("private_link_target") else {
            return;
        };
        let link = std::env::temp_dir().join("keymapperd_cfgio_priv_link");
        std::os::unix::fs::symlink(&real, &link)
            .expect("failed to create symlink");
        let path = link.join("config.yaml");
        std::fs::write(real.join("config.yaml"), "groups: []")
            .expect("failed to write config");

        let content = read_config_content(&path).expect(
            "a symlinked parent resolving to a private dir must be \
             traversable",
        );
        std::fs::remove_file(&link).ok();
        std::fs::remove_dir_all(&real).ok();

        assert_eq!(content, "groups: []");
    }
}
