// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::path::{Path, PathBuf};

const APP_NAME: &str = "keymapperd";
const CONFIG_FILE: &str = "config.toml";

/// Search standard platform directories for the user configuration file.
///
/// Search order:
/// 1. Platform-specific application config directory
/// 2. Current working directory (development convenience)
///
/// Returns `None` when no configuration file exists in any search location.
pub fn find_config_path() -> Option<PathBuf> {
    let candidate = |dir: &Path| dir.join(CONFIG_FILE);

    // Platform-specific application directory.
    if let Some(dir) = platform_config_dir() {
        let path = candidate(&dir);
        if path.is_file() {
            return Some(path);
        }
    }

    // CWD (development / manual invocation).
    let path = candidate(Path::new("."));
    if path.is_file() {
        return Some(path);
    }

    None
}

/// Print the directories searched and the expected file name so that the
/// user knows where to create their configuration.
pub fn print_search_locations() {
    eprintln!(
        "No configuration file found ({CONFIG_FILE}). \
         Please create it in one of the following locations:"
    );

    if let Some(dir) = platform_config_dir() {
        eprintln!("  1. {}", dir.display());
    }
    eprintln!("  2. Current working directory");
}

// ---------------------------------------------------------------------------
// Platform-specific config directory resolution
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn platform_config_dir() -> Option<PathBuf> {
    // ~/Library/Application Support/keymapperd
    dirs::home_dir().map(|h| h.join("Library").join("Application Support").join(APP_NAME))
}

#[cfg(target_os = "linux")]
fn platform_config_dir() -> Option<PathBuf> {
    // $XDG_CONFIG_HOME/keymapperd  (or ~/.config/keymapperd)
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir()?.join(".config"))
        .map(|d| d.join(APP_NAME))
}

#[cfg(target_os = "windows")]
fn platform_config_dir() -> Option<PathBuf> {
    // %APPDATA%\keymapperd
    dirs::config_dir().map(|d| d.join(APP_NAME))
}
