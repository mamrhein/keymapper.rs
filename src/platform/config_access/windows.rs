// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows enforcement of the hardened config read.
//!
//! Windows has no analogue of `O_NOFOLLOW` or of the POSIX mode bits:
//! reparse-point traversal and file trust are governed by the object's
//! DACL, not by flags or writable bits a reader can inspect.  The open is
//! therefore plain, and the metadata trust check is a no-op; the caller's
//! preceding symlink-metadata refusal is the available guard, and the OS's
//! own ACL enforcement is the real one.

use std::{
    fs::{File, Metadata, OpenOptions},
    path::Path,
};

use crate::common::config_io::{ConfigReadError, ConfigWriteError};

/// Open *path* for reading.  Without an `O_NOFOLLOW` equivalent, only the
/// error mapping matters here: a missing file becomes
/// [`ConfigReadError::NotFound`], anything else keeps its distinct I/O
/// error.
pub(crate) fn open_config_file(path: &Path) -> Result<File, ConfigReadError> {
    File::open(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ConfigReadError::NotFound
        } else {
            ConfigReadError::Io(err)
        }
    })
}

/// No-op: ownership and world-writable mode bits do not exist in the
/// Windows ACL model, so there is nothing to check.
pub(crate) fn check_file_trust(
    _metadata: &Metadata,
) -> Result<(), ConfigReadError> {
    Ok(())
}

/// Create a temp file for an atomic config write.
///
/// Windows has no `O_NOFOLLOW` or POSIX mode bits; `create_new(true)`
/// maps to `CREATE_NEW`, which fails if the file already exists, giving
/// the same exclusive-creation guarantee as `O_EXCL`.  The symlink
/// refusal is handled at the `config_io` layer via `symlink_metadata`,
/// mirroring the read path.
pub(crate) fn open_temp_file_for_write(
    path: &Path,
) -> Result<File, ConfigWriteError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(ConfigWriteError::Io)
}

/// No-op: Windows has no POSIX mode bits or symlink-following concerns
/// for `create_dir_all`.
pub(crate) fn verify_parent_chain_for_create(
    _path: &Path,
) -> Result<(), ConfigReadError> {
    Ok(())
}
