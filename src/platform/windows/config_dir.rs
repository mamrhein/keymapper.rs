// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows per-user configuration base directory resolution.

use std::path::PathBuf;

/// Return the OS-specific per-user configuration base directory, without the
/// application name.
///
/// On Windows this is `%APPDATA%`.  The directory may not exist yet.
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir()
}

/// Windows has no console-user indirection.
pub(crate) fn console_user_config_dir() -> Option<PathBuf> {
    None
}

/// The daemon's directory under the per-user local app-data root
/// (`%LOCALAPPDATA%\keymapperd`), shared by the log directory and the
/// control-pipe publish file so both sides resolve the same location.
pub(crate) fn local_app_data_dir() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join(crate::common::paths::APP_DIR_NAME))
}
