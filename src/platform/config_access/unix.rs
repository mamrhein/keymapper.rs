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
//! ownership reasoning lives here: the openat-style per-component path
//! resolution with fd-based verification of each traversed directory
//! (R3-2), the `O_NOFOLLOW` open of the final component, and the
//! ownership/world-writable checks on the open descriptor's metadata.

use std::{
    ffi::CString,
    fs::{File, Metadata, OpenOptions},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawFd, FromRawFd},
    },
    path::{Component, Path},
};

use crate::common::config_io::{ConfigReadError, ConfigWriteError};

/// Recorded for each traversed ancestor directory so that, when the daemon
/// runs as root, we can verify ancestor ownership against the config file's
/// owner (R3-2 fix 2).  The mode is checked inline during traversal; only the
/// uid is retained for the post-open ownership check.
struct AncestorInfo {
    path: String,
    uid: u32,
}

/// Open *path* for reading, refusing to follow a symlink in the final
/// component and verifying every traversed directory by file descriptor.
///
/// The path is resolved component by component with `openat` (R3-2 fix 1):
/// each intermediate directory is opened with `O_DIRECTORY` and immediately
/// `fstat`-verified on the resulting fd, so a symlink or mode swap between a
/// by-path `stat` and the later `open` is impossible — the fd is already
/// anchored to the directory that was inspected.  On Linux, when the kernel
/// supports `openat2`, an independent `O_NOFOLLOW` re-open of the final
/// component is run as additional defense; when `openat2` is absent (older
/// kernels, macOS) the per-component `openat` ladder is the sole mechanism —
/// the portable fallback.
///
/// When the process is root, each ancestor directory must additionally be
/// owned by the config file's owner or root (R3-2 fix 2), closing the gap
/// where the file-ownership check is relaxed for root.
///
/// The final component is opened with `O_NOFOLLOW`: a symlink planted in the
/// final position between the caller's symlink-metadata check and this open
/// is rejected with `ELOOP` rather than followed.
pub(crate) fn open_config_file(path: &Path) -> Result<File, ConfigReadError> {
    let current_uid = unsafe { libc::getuid() };
    let is_root = current_uid == 0;

    let (file, ancestors) = resolve_config_path(path)?;

    // When running as root the file-ownership check in `check_file_trust` is
    // deliberately relaxed (a root daemon reads configs owned by regular
    // users).  Anchor the relaxed check by requiring every ancestor directory
    // to be owned by the config file's owner or root — an unprivileged user
    // cannot otherwise redirect a root daemon's config read through a
    // directory they control.
    if is_root && !ancestors.is_empty() {
        let metadata =
            file.metadata().map_err(|_| ConfigReadError::Metadata)?;
        let file_uid = metadata.uid();
        for ancestor in &ancestors {
            if ancestor.uid != file_uid && ancestor.uid != 0 {
                return Err(ConfigReadError::UntrustedParentDir {
                    path: ancestor.path.clone(),
                    reason: "ancestor not owned by config owner or root",
                });
            }
        }
    }

    // Defense-in-depth: on Linux kernels with `openat2`, independently verify
    // the final component is not a symlink via a different kernel path.  If
    // `openat2` is unavailable the per-component `openat` ladder above is
    // sufficient; if it detects a symlink that appeared between our open and
    // the check, fail closed.
    #[cfg(target_os = "linux")]
    verify_final_component_with_openat2(path)?;

    Ok(file)
}

/// Resolve *path* component by component using `openat`, returning the
/// final file descriptor and the list of traversed ancestor directories.
///
/// Each intermediate component is opened with `O_NOFOLLOW`-free `openat` (so
/// symlinked parent directories remain traversable, matching existing
/// behaviour) and then `fstat`-verified on the fd: not a non-directory, not
/// world-writable without the sticky bit.  Because the fd is anchored to the
/// resolved directory, a TOCTOU swap of the path between components cannot
/// redirect the traversal.
///
/// The final component is opened with `O_NOFOLLOW` so a symlink in the final
/// position is rejected with `ELOOP` -> [`ConfigReadError::Symlink`].
fn resolve_config_path(
    path: &Path,
) -> Result<(File, Vec<AncestorInfo>), ConfigReadError> {
    let mut ancestors: Vec<AncestorInfo> = Vec::new();
    let mut dir_files: Vec<File> = Vec::new();

    // Starting dirfd and accumulated prefix for error messages.
    let mut dirfd = libc::AT_FDCWD;
    let mut prefix = String::new();

    let components: Vec<Component> = path.components().collect();
    if components.is_empty() {
        return Err(ConfigReadError::Io(std::io::Error::from_raw_os_error(
            libc::ENOENT,
        )));
    }

    // Absolute paths start from "/".
    let start = if components[0] == Component::RootDir {
        let fd = open_directory(libc::AT_FDCWD, b"/")?;
        dir_files.push(fd);
        prefix.push('/');
        dirfd = dir_files.last().unwrap().as_raw_fd();
        1
    } else {
        0
    };

    let remaining = &components[start..];
    if remaining.is_empty() {
        // The path is just "/" — no file component.  The caller's
        // `is_file()` check will reject a directory, but return early with
        // NotFound to avoid a confusing error.
        return Err(ConfigReadError::NotFound);
    }

    for (i, comp) in remaining.iter().enumerate() {
        let is_last = i + start == components.len() - 1;
        let flags = if is_last {
            libc::O_NOFOLLOW
        } else {
            libc::O_DIRECTORY
        };

        let name = match comp {
            Component::Normal(n) => n,
            Component::CurDir => continue, // "./" is a no-op.
            Component::ParentDir => std::ffi::OsStr::new(".."),
            // RootDir was handled above; Prefix is Windows-only.
            _ => continue,
        };

        if prefix.is_empty() || prefix.ends_with('/') {
            prefix.push_str(&name.to_string_lossy());
        } else {
            prefix.push('/');
            prefix.push_str(&name.to_string_lossy());
        }

        let name_cstr = os_str_to_cstring(name)?;
        let raw_fd = unsafe { libc::openat(dirfd, name_cstr.as_ptr(), flags) };
        if raw_fd < 0 {
            let err = std::io::Error::last_os_error();
            return Err(map_open_error(&prefix, err, is_last));
        }

        // Safety: raw_fd was just returned by openat and is owned by us.
        let file = unsafe { File::from_raw_fd(raw_fd) };

        if !is_last {
            let metadata =
                file.metadata().map_err(|_| ConfigReadError::Metadata)?;

            if !metadata.is_dir() {
                return Err(ConfigReadError::UntrustedParentDir {
                    path: prefix.clone(),
                    reason: "not a directory",
                });
            }

            let mode = metadata.mode() as libc::mode_t;
            if (mode & libc::S_IWOTH) != 0 && (mode & libc::S_ISVTX) == 0 {
                return Err(ConfigReadError::UntrustedParentDir {
                    path: prefix.clone(),
                    reason: "world-writable without sticky bit",
                });
            }

            ancestors.push(AncestorInfo {
                path: prefix.clone(),
                uid: metadata.uid(),
            });
        }

        // Keep the fd alive for the next openat in the chain.
        dir_files.push(file);
        dirfd = dir_files.last().unwrap().as_raw_fd();
    }

    // The last entry is the config file; pop it so the intermediate directory
    // fds are closed when `dir_files` is dropped at function exit.
    let file = dir_files.pop().unwrap();
    Ok((file, ancestors))
}

/// Open a directory relative to *dirfd* with `O_DIRECTORY` and return it as a
/// [`File`].
fn open_directory(
    dirfd: libc::c_int,
    name: &[u8],
) -> Result<File, ConfigReadError> {
    let cstr = CString::new(name).map_err(|_| {
        ConfigReadError::Io(std::io::Error::from_raw_os_error(libc::EINVAL))
    })?;
    let raw_fd =
        unsafe { libc::openat(dirfd, cstr.as_ptr(), libc::O_DIRECTORY) };
    if raw_fd < 0 {
        let err = std::io::Error::last_os_error();
        return Err(map_open_error("/", err, false));
    }
    // Safety: raw_fd was just returned by openat and is owned by us.
    Ok(unsafe { File::from_raw_fd(raw_fd) })
}

/// Convert a Unix `OsStr` path component to a `CString`, rejecting embedded
/// NUL bytes (which cannot occur in real paths but are possible in
/// `OsStr` on Unix).
fn os_str_to_cstring(
    name: &std::ffi::OsStr,
) -> Result<CString, ConfigReadError> {
    CString::new(name.as_bytes()).map_err(|_| {
        ConfigReadError::Io(std::io::Error::from_raw_os_error(libc::EINVAL))
    })
}

/// Map an `openat` error to the appropriate [`ConfigReadError`].
///
/// `ELOOP` means a symlink was in the opened position; `ENOENT` means the
/// component did not exist; `ENOTDIR` means a non-directory was found where
/// a directory was required.
fn map_open_error(
    path: &str,
    err: std::io::Error,
    is_last: bool,
) -> ConfigReadError {
    match err.raw_os_error() {
        Some(libc::ELOOP) => ConfigReadError::Symlink,
        Some(libc::ENOENT) => {
            if is_last {
                ConfigReadError::NotFound
            } else {
                ConfigReadError::UntrustedParentDir {
                    path: path.to_string(),
                    reason: "missing",
                }
            }
        }
        Some(libc::ENOTDIR) => {
            if is_last {
                ConfigReadError::Io(err)
            } else {
                ConfigReadError::UntrustedParentDir {
                    path: path.to_string(),
                    reason: "not a directory",
                }
            }
        }
        _ => ConfigReadError::Io(err),
    }
}

/// On Linux kernels that support `openat2`, independently re-open *path*
/// with `O_NOFOLLOW` and verify the final component is not a symlink via a
/// different kernel path than the per-component `openat` ladder.
///
/// Returns `Ok(())` when the cross-check passes or `openat2` is unavailable
/// (`ENOSYS`), and an error when the cross-check detects a problem (e.g. a
/// symlink planted in the final position after our `openat` succeeded).
///
/// When `openat2` is absent the per-component `openat` ladder is the sole
/// mechanism — this is the portable fallback.
#[cfg(target_os = "linux")]
fn verify_final_component_with_openat2(
    path: &Path,
) -> Result<(), ConfigReadError> {
    // libc::open_how is #[non_exhaustive], so we define a layout-compatible
    // struct to pass to the openat2 syscall.
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    let how = OpenHow {
        flags: libc::O_NOFOLLOW as u64,
        mode: 0,
        resolve: 0, // RESOLVE_NO_SYMLINKS omitted so symlinked parent dirs
    };

    let cstr = match CString::new(path.to_string_lossy().as_bytes()) {
        Ok(s) => s,
        Err(_) => {
            return Err(ConfigReadError::Io(
                std::io::Error::from_raw_os_error(libc::EINVAL),
            ));
        }
    };

    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            cstr.as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };

    if fd == -1 {
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            // openat2 not supported — fall back to the openat ladder.
            Some(libc::ENOSYS) => return Ok(()),
            // File deleted between our open and the check; the openat
            // already verified the fd, so do not fail.
            Some(libc::ENOENT) => return Ok(()),
            // A symlink appeared in the final position — fail closed.
            Some(libc::ELOOP) => return Err(ConfigReadError::Symlink),
            // Any other error is not a symlink concern; the per-component
            // openat is authoritative.
            _ => return Ok(()),
        }
    }

    // Safety: fd was returned by openat2 and is owned by us.
    unsafe { libc::close(fd as libc::c_int) };
    Ok(())
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

    // Security check: file is not world-readable unless owned by root.
    // A root-owned config in a root-owned directory is fine to be
    // world-readable; a user-owned world-readable config leaks mapping
    // rules to other local users on a multi-user system.
    if uid != 0 && (mode & libc::S_IROTH) != 0 {
        return Err(ConfigReadError::WorldReadable);
    }
    Ok(())
}

/// Parent-chain verification for the config create path.
///
/// Unlike [`resolve_config_path`], which opens each component via `openat`
/// and verifies by fd, this pre-check uses by-path `stat` because the
/// directories do not yet exist at the time of the check —
/// `create_dir_all` will create them.  It allows non-existent directories
/// while still rejecting existing world-writable-without-sticky-bit
/// ancestors or non-directory components.
///
/// Used by `config_cmd::create` before `create_dir_all`: the existing
/// ancestors must be trusted (no world-writable-without-sticky-bit, no
/// non-directory component), but directories that do not yet exist are
/// skipped — they will be created by `create_dir_all` with the process
/// umask and are trusted by construction.
pub(crate) fn verify_parent_chain_for_create(
    path: &Path,
) -> Result<(), ConfigReadError> {
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
                // Does not exist yet; create_dir_all will make it.  Skip
                // this and any deeper ancestors.
                continue;
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
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("failed to set permissions");
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
    fn world_readable_is_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let path = write_temp("world_readable", "groups: []");
        std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(0o644),
        )
        .expect("failed to chmod");

        let err = read_config_content(&path).unwrap_err();
        std::fs::remove_file(&path).ok();

        assert!(
            matches!(err, ConfigReadError::WorldReadable),
            "expected WorldReadable, got {err:?}"
        );
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
        use std::os::unix::fs::PermissionsExt;

        let Some(real) = private_temp_subdir("private_link_target") else {
            return;
        };
        let link = std::env::temp_dir().join("keymapperd_cfgacc_priv_link");
        std::os::unix::fs::symlink(&real, &link)
            .expect("failed to create symlink");
        let path = link.join("config.yaml");
        let target = real.join("config.yaml");
        std::fs::write(&target, "groups: []").expect("failed to write config");
        std::fs::set_permissions(
            &target,
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("failed to set permissions");

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

    #[test]
    fn create_chain_allows_nonexistent_dirs() {
        let dir =
            std::env::temp_dir().join("keymapperd_cfgacc_create_chain_ok");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir(&dir).expect("failed to create base dir");
        // A path under the base that does not exist yet — the pre-check
        // must allow it because create_dir_all will make it.
        let path = dir.join("new_subdir").join("config.yaml");

        verify_parent_chain_for_create(&path).expect(
            "existing private ancestor chain must pass the create pre-check",
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_chain_rejects_world_writable_dir() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join("keymapperd_cfgacc_create_chain_ww");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir(&dir).expect("failed to create base dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
            .expect("failed to chmod");
        let path = dir.join("config.yaml");

        let err = verify_parent_chain_for_create(&path).unwrap_err();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
            .expect("failed to restore mode");
        std::fs::remove_dir_all(&dir).ok();

        assert!(
            matches!(err, ConfigReadError::UntrustedParentDir { .. }),
            "expected UntrustedParentDir, got {err:?}",
        );
    }

    /// Race regression test for R3-2.
    ///
    /// A background thread continuously swaps a symlinked directory component
    /// in the config path between a private (trusted) target and a
    /// world-writable-without-sticky-bit (untrusted) target.  The main thread
    /// repeatedly reads the config through the swapped symlink.  With the old
    /// by-path `verify_parent_chain` + path-based `open` there was a TOCTOU
    /// window: the `stat` could see the trusted directory while the later
    /// `open` followed the swapped symlink into the untrusted one.  The
    /// per-component `openat` resolution closes that window because each
    /// directory is opened and `fstat`-verified on its own fd — the fd is
    /// anchored before the next component is resolved.
    ///
    /// The test fails if the untrusted content is ever observed, proving the
    /// race is eliminated.
    #[test]
    fn race_config_read_no_swap_to_untrusted() {
        use std::{
            os::unix::fs::PermissionsExt,
            sync::atomic::{AtomicBool, Ordering},
            time::{Duration, Instant},
        };

        // The root daemon relaxes the file-ownership check; this race
        // regression targets the non-root TOCTOU window.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let Some(base) = private_temp_subdir("race_swap") else {
            return;
        };

        // trusted/ — private directory with a config file.
        let trusted = base.join("trusted");
        std::fs::create_dir(&trusted).expect("failed to create trusted dir");
        std::fs::set_permissions(
            &trusted,
            std::fs::Permissions::from_mode(0o700),
        )
        .expect("failed to chmod trusted");
        let trusted_cfg = trusted.join("config.yaml");
        std::fs::write(&trusted_cfg, "groups: []")
            .expect("failed to write trusted config");
        std::fs::set_permissions(
            &trusted_cfg,
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("failed to chmod trusted config");

        // untrusted/ — world-writable without sticky bit, with a config
        // whose content is distinguishable so a leak is detectable.
        let untrusted = base.join("untrusted");
        std::fs::create_dir(&untrusted)
            .expect("failed to create untrusted dir");
        std::fs::set_permissions(
            &untrusted,
            std::fs::Permissions::from_mode(0o777),
        )
        .expect("failed to chmod untrusted");
        let untrusted_cfg = untrusted.join("config.yaml");
        std::fs::write(&untrusted_cfg, "groups: []\nLEAKED")
            .expect("failed to write untrusted config");
        std::fs::set_permissions(
            &untrusted_cfg,
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("failed to chmod untrusted config");

        // link → trusted initially.
        let link = base.join("link");
        std::os::unix::fs::symlink(&trusted, &link)
            .expect("failed to create symlink");

        use std::sync::Arc;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let swap_link = link.clone();
        let swap_trusted = trusted.clone();
        let swap_untrusted = untrusted.clone();

        // Background thread: rapidly swap the symlink between trusted and
        // untrusted, simulating an attacker who can rename-swap a directory
        // component in the config path.
        let handle = std::thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                std::fs::remove_file(&swap_link).ok();
                std::os::unix::fs::symlink(&swap_untrusted, &swap_link).ok();
                std::thread::yield_now();
                std::fs::remove_file(&swap_link).ok();
                std::os::unix::fs::symlink(&swap_trusted, &swap_link).ok();
                std::thread::yield_now();
            }
        });

        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline {
            let path = link.join("config.yaml");
            match read_config_content(&path) {
                Ok(content) => {
                    assert!(
                        !content.contains("LEAKED"),
                        "read leaked untrusted content through a swapped
                             symlink"
                    );
                }
                Err(ConfigReadError::UntrustedParentDir { .. }) => {}
                Err(ConfigReadError::Io(_)) => {}
                Err(ConfigReadError::NotFound) => {}
                Err(ConfigReadError::Symlink) => {}
                Err(_) => {}
            }
        }

        stop.store(true, Ordering::Relaxed);
        handle.join().expect("background thread panicked");

        std::fs::remove_file(&link).ok();
        std::fs::remove_dir_all(&base).ok();
    }
}
