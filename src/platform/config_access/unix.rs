// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Unix enforcement of the hardened config read.
//!
//! Serves Linux and macOS (the facet's fault line is unix vs. the rest,
//! mirroring [`endpoint`](super::super::endpoint)).  All mode-bit and
//! ownership reasoning lives here: the parent-directory chain inspection
//! (SEC-19), the `O_NOFOLLOW` open, and the ownership/world-writable checks
//! on the open descriptor's metadata.

use std::{
    fs::{File, Metadata, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

use crate::common::config_io::{ConfigReadError, ConfigWriteError};

/// Open *path* for reading, refusing to follow a symlink.
///
/// The parent-directory chain is verified first (see
/// [`verify_parent_chain`]), then the file is opened with `O_NOFOLLOW` so a
/// symlink planted after the caller's symlink-metadata check is rejected
/// with `ELOOP` rather than followed.
pub(crate) fn open_config_file(path: &Path) -> Result<File, ConfigReadError> {
    verify_parent_chain(path)?;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|err| {
            // `O_NOFOLLOW` rejects a symlink in the final position
            // with ELOOP: that is the symlink refusal, most likely a
            // link planted after the caller's symlink-metadata check.
            if err.raw_os_error() == Some(libc::ELOOP) {
                ConfigReadError::Symlink
            } else if err.kind() == std::io::ErrorKind::NotFound {
                ConfigReadError::NotFound
            } else {
                ConfigReadError::Io(err)
            }
        })
}

/// Create a temp file for an atomic config write.
///
/// The file is created with `O_CREAT | O_EXCL` (exclusive — fails if the
/// name already exists, so an existing file or symlink can never be
/// clobbered), `O_NOFOLLOW` (rejects a symlink at the temp path with
/// `ELOOP`), and mode `0600` (never world-readable, independent of umask).
/// The caller writes, `fsync`s, and `rename(2)`s the file over the
/// target; on failure the caller removes the temp file.
pub(crate) fn open_temp_file_for_write(
    path: &Path,
) -> Result<File, ConfigWriteError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(path)
        .map_err(|err| {
            if err.raw_os_error() == Some(libc::ELOOP) {
                ConfigWriteError::Symlink
            } else {
                ConfigWriteError::Io(err)
            }
        })
}

/// Check the metadata of the already-open config file against the Unix
/// trust rules: owned by the current user (unless running as root) and not
/// world-writable.
pub(crate) fn check_file_trust(
    metadata: &Metadata,
) -> Result<(), ConfigReadError> {
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
    Ok(())
}

/// Verify that every ancestor directory of *path* can be trusted (SEC-19).
///
/// A world-writable directory without the sticky bit, or a component that
/// resolves to a non-directory, may let a user who does not own the config
/// redirect the path to a file they control, so such a chain aborts the
/// read.  Components are inspected with symlinks resolved: trust attaches
/// to the directory that is actually traversed, never to the link or name
/// used to reach it.  Sticky-bit protected shared directories (e.g. `/tmp`,
/// or the system temp directory on macOS) are exempted because custom CLI
/// config paths under them are legitimate and the daemon's config search
/// path never lies inside one.
fn verify_parent_chain(path: &Path) -> Result<(), ConfigReadError> {
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
        if (dir_mode & libc::S_IWOTH) != 0 && (dir_mode & libc::S_ISVTX) == 0 {
            return Err(ConfigReadError::UntrustedParentDir {
                path: dir.display().to_string(),
                reason: "world-writable without sticky bit",
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::common::config_io::read_config_content;

    /// Write *content* to a temp file and return its path.  Each invocation
    /// gets a unique filename keyed by *label* to avoid races when tests run
    /// in parallel.  The caller must delete the file (or let the process
    /// exit).
    fn write_temp(label: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("keymapperd_cfgacc_{}.yaml", label));
        std::fs::write(&path, content).expect("failed to write temp config");
        path
    }

    /// Create a unique private directory below the temp dir, or return
    /// `None` if the temp dir itself has no sticky bit (its own
    /// world-writable mode would make every child chain untrusted anyway).
    fn private_temp_subdir(label: &str) -> Option<PathBuf> {
        let temp = std::env::temp_dir();
        let mode = std::fs::metadata(&temp)
            .expect("failed to stat temp dir")
            .mode() as libc::mode_t;
        // If the temp dir itself would not pass the chain check, the
        // platform setup is unusual; skip rather than fail spuriously.
        if (mode & libc::S_IWOTH) != 0 && (mode & libc::S_ISVTX) == 0 {
            return None;
        }
        let dir = temp.join(format!("keymapperd_cfgacc_{label}_d"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir(&dir).expect("failed to create temp dir");
        Some(dir)
    }

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

    #[test]
    fn open_config_file_refuses_symlink_via_eloop() {
        // A dangling symlink: without `O_NOFOLLOW` the open would fail with
        // ENOENT; the ELOOP -> Symlink mapping proves the flag rejected the
        // link itself, which is the guard against a link planted after the
        // caller's symlink-metadata check.
        let link =
            std::env::temp_dir().join("keymapperd_cfgacc_dangling.link");
        std::fs::remove_file(&link).ok();
        std::os::unix::fs::symlink("/nonexistent/config.yaml", &link)
            .expect("failed to create symlink");

        let err = open_config_file(&link).unwrap_err();
        std::fs::remove_file(&link).ok();

        assert!(matches!(err, ConfigReadError::Symlink));
    }

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
        let link = std::env::temp_dir().join("keymapperd_cfgacc_ww_link");
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

    #[test]
    fn symlinked_parent_to_private_dir_is_followed() {
        let Some(real) = private_temp_subdir("private_link_target") else {
            return;
        };
        let link = std::env::temp_dir().join("keymapperd_cfgacc_priv_link");
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

    #[test]
    fn untraversable_parent_dir_keeps_the_io_error() {
        use std::os::unix::fs::PermissionsExt;

        // The superuser ignores directory execute permissions, so the
        // setup below cannot produce EACCES when running as root.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let Some(dir) = private_temp_subdir("nox_parent") else {
            return;
        };
        let path = dir.join("config.yaml");
        std::fs::write(&path, "groups: []").expect("failed to write config");
        // Revoking every permission bit on the parent makes every path
        // operation under it fail with EACCES (denied traversal) rather
        // than ENOENT; that distinction must survive into the error.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000))
            .expect("failed to chmod dir");

        let err = read_config_content(&path).unwrap_err();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
            .expect("failed to restore the dir mode");
        std::fs::remove_dir_all(&dir).ok();

        assert!(
            matches!(err, ConfigReadError::Io(_)),
            "expected a preserved I/O error, got {err:?}",
        );
    }
}
