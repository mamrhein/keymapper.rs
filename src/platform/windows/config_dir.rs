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
