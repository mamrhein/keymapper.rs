// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Hardened reading and writing of the configuration file.
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
//! The same path is hardened for writes: [`write_config_atomic`] writes the
//! content to a temp file created with `O_NOFOLLOW | O_EXCL` and mode
//! `0600` (Unix), `fsync`s it, then `rename(2)`s it over the target — an
//! atomic swap that can never leave a partially-written config behind.
//! The temp file is removed on any failure, so the original config is
//! always left untouched.  The target path itself is rejected if it is a
//! symlink, closing the write-back TOCTOU race (SEC-1).
//!
//! This module owns the shared sequence and the platform-neutral checks;
//! the OS-specific enforcement (on Unix the `O_NOFOLLOW` open, the
//! parent-directory-chain inspection, and the ownership and mode checks;
//! elsewhere a plain open with no mode checks) lives in
//! [`crate::platform::config_access`].

use std::{
    io::{Read, Write},
    path::Path,
};

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

    /// The config file is world-readable and not owned by root (Unix
    /// only).  A user-owned world-readable config leaks keyboard mapping
    /// rules to other local users on a multi-user system.
    #[error("config file is world-readable; use `chmod 600` to fix")]
    WorldReadable,

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

/// Error returned when the config file cannot be written safely.
#[derive(Debug, Error)]
pub enum ConfigWriteError {
    /// The config path is a symlink; the write is refused to prevent
    /// following a symlink planted between inspection and write (SEC-1).
    #[error("config file is a symlink")]
    Symlink,

    /// The config file could not be created, written, or renamed for a
    /// reason other than a symlink (e.g. permission denied, disk full).
    /// The distinct I/O error kind is preserved for diagnostics.
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

/// Write *content* to *path* atomically and with hardening.
///
/// The write is performed via a temp file that is created with `O_NOFOLLOW`
/// and `O_EXCL` (Unix) and mode `0600`, so it can never clobber an existing
/// file and is never world-readable.  After the content is written and
/// `fsync`'d, the temp file is `rename(2)`'d over *path* — an atomic swap on
/// every supported platform that can never leave a partially-written config
/// behind.  If any step fails the temp file is removed, so the original
/// config is always left untouched.
///
/// The target *path* itself is rejected if it is a symlink, closing the
/// write-back TOCTOU race: a symlink planted between a read and this write
/// cannot be followed.
///
/// `create` and `add` use this helper so the write path cannot drift from
/// the read path's hardening.
pub fn write_config_atomic(
    path: &Path,
    content: &str,
) -> Result<(), ConfigWriteError> {
    // Security check: reject a symlink in the target position.  A missing
    // target is fine (we are creating it); only an existing symlink is
    // refused.  This mirrors the read path's symlink-metadata refusal and
    // closes the write-back TOCTOU window (SEC-1).
    if let Ok(sym_meta) = std::fs::symlink_metadata(path)
        && sym_meta.file_type().is_symlink()
    {
        return Err(ConfigWriteError::Symlink);
    }

    // The temp file lives in the same directory as the target so that
    // `rename(2)` is a same-filesystem operation (cross-device rename fails).
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp_path = parent.join(temp_file_name(path));

    // Create the temp file with platform-specific hardening (Unix:
    // O_NOFOLLOW | O_EXCL, mode 0600).  On any failure the temp file is
    // removed and the original config is untouched.
    let mut file = config_access::open_temp_file_for_write(&temp_path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp_path);
    })?;

    // Write the content and fsync before renaming, so the rename is the
    // last step and the new file is durable when it becomes visible.
    let write_result: Result<(), std::io::Error> = (|| {
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    // The file handle must be closed before rename: Windows rejects
    // renaming an open file, and Unix is happiest with a closed fd too.
    drop(file);
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(ConfigWriteError::Io(err));
    }

    // Atomically replace the target.  `rename(2)` does not follow symlinks
    // for the destination, so even if the target became a symlink between
    // the check above and this call, the symlink itself is replaced rather
    // than followed.
    if let Err(err) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(ConfigWriteError::Io(err));
    }

    info!("Wrote config to {}", path.display());
    Ok(())
}

/// Generate a unique temp-file name for *path*'s sibling temp file.
///
/// The name is hidden (leading dot), includes the target's file name for
/// debuggability, and is suffixed with the PID and a nanosecond timestamp so
/// collisions across parallel writes are practically impossible.  The
/// `O_EXCL` flag in [`crate::platform::config_access`] is the real
/// uniqueness guarantee: even if the name collides, the open fails rather
/// than clobbers.
fn temp_file_name(path: &Path) -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".to_string());
    format!(".{base}.{pid:x}.{nanos:x}.tmp")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------</arg_value></tool_call> File path: keymapper.rs/src/common/config_io.rs</arg_value></tool_call> File path: keymapper.rs/src/common/config_io.rs</arg_value></tool_call><tool_call>edit_file<arg_key>path</arg_key><arg_value>keymapper.rs/src/common/config_io.rs</arg_value><arg_key>edits</arg_key><arg_value>[{

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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode(0o600),
            )
            .expect("failed to set permissions");
        }
        path
    }

    #[test]
    fn writes_content_atomically() {
        let path = write_temp("atomic", "groups: []");
        write_config_atomic(&path, "groups: []").expect("atomic write");
        let read = read_config_content(&path).expect("should read back");
        std::fs::remove_file(&path).ok();

        assert_eq!(read, "groups: []");
    }

    #[test]
    fn atomic_write_creates_new_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("keymapperd_config_io_create.yaml");
        std::fs::remove_file(&path).ok();

        write_config_atomic(&path, "groups: []").expect("atomic create");
        let read = read_config_content(&path).expect("should read back");
        std::fs::remove_file(&path).ok();

        assert_eq!(read, "groups: []");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_rejects_symlink() {
        let target = write_temp("symlink_target", "groups: []");
        let link = target.with_file_name(format!(
            "{}.link", target.file_name().unwrap().to_string_lossy()
        ));
        std::os::unix::fs::symlink(&target, &link).expect("failed to create symlink");

        let err = write_config_atomic(&link, "groups: []").unwrap_err();
        std::fs::remove_file(&link).ok();
        std::fs::remove_file(&target).ok();

        assert!(matches!(err, ConfigWriteError::Symlink));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_creates_file_with_0600() {
        use std::os::unix::fs::PermissionsExt;

        let path = write_temp("perms", "old content");
        write_config_atomic(&path, "new content").expect("atomic write");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode() as libc::mode_t;
        std::fs::remove_file(&path).ok();

        assert_eq!(
            mode & 0o777, 0o600,
            "config file must be 0600, got {:o}", mode & 0o777
        );
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let path = write_temp("overwrite", "old content");
        write_config_atomic(&path, "new content").expect("atomic overwrite");
        let read = read_config_content(&path).expect("should read back");
        std::fs::remove_file(&path).ok();

        assert_eq!(read, "new content");
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
