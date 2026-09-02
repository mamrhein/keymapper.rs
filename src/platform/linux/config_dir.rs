// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux per-user configuration base directory resolution.

use std::path::PathBuf;

/// Return the OS-specific per-user configuration base directory, without the
/// application name.
///
/// Uses `$XDG_CONFIG_HOME` when it is set, otherwise falls back to
/// `~/.config`.  The directory may not exist yet.
pub fn config_dir() -> Option<PathBuf> {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
}
