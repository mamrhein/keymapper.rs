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
const CONFIG_FILE: &str = "config.yaml";

/// Check whether a path is a symbolic link (does not follow the link).
fn is_symlink(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path).ok(),
        Some(m) if m.file_type().is_symlink()
    )
}

/// Returns the canonical path where the configuration file should reside.
/// This is the platform-specific application config directory plus the
/// default file name.  The directory may not exist yet.
pub fn default_config_path() -> Option<PathBuf> {
    platform_config_dir().map(|d| d.join(CONFIG_FILE))
}

/// Search standard platform directories for the user configuration file.
///
/// Search order:
/// 1. Platform-specific application config directory
/// 2. Current working directory (development convenience)
///
/// Symbolic links are rejected; `config.yaml` must be a regular file.
/// Returns `None` when no configuration file exists in any search location.
pub fn find_config_path() -> Option<PathBuf> {
    let candidate = |dir: &Path| dir.join(CONFIG_FILE);

    // Platform-specific application directory.
    if let Some(dir) = platform_config_dir() {
        let path = candidate(&dir);
        if path.is_file() && !is_symlink(&path) {
            return Some(path);
        }
    }

    // CWD (development / manual invocation).
    let path = candidate(Path::new("."));
    if path.is_file() && !is_symlink(&path) {
        return Some(path);
    }

    None
}

/// Search for the configuration file, returning a clear error if the found
/// file is a symbolic link.  This is the variant used by the daemon and CLI
/// to provide actionable feedback.
///
/// Returns:
/// - `Ok(path)` when a valid config file is found.
/// - `Err("not found")` with search locations printed to stderr.
/// - `Err(symlink message)` when the found file is a symbolic link.
pub fn find_config_path_strict() -> Result<PathBuf, String> {
    let candidate = |dir: &Path| dir.join(CONFIG_FILE);

    for dir in search_dirs() {
        let path = candidate(&dir);
        if !path.is_file() {
            continue;
        }

        if is_symlink(&path) {
            return Err(format!(
                "config file {} is a symbolic link and will not be followed",
                path.display(),
            ));
        }

        return Ok(path);
    }

    print_search_locations();
    Err("configuration file not found".to_string())
}

/// Iterate over the search locations in priority order.
fn search_dirs() -> impl Iterator<Item = PathBuf> {
    platform_config_dir().into_iter().chain([cwd_path()])
}

/// Return the current working directory, or `None` if it cannot be determined.
fn cwd_path() -> Option<PathBuf> {
    std::env::current_dir().ok()
}

/// Print the directories searched and the expected file name so that the
/// user knows where to create their configuration.
pub fn print_search_locations() {
    eprintln!(
        "No configuration file found ({CONFIG_FILE}). Please create it in \
         one of the following locations:"
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
    dirs::home_dir()
        .map(|h| h.join("Library").join("Application Support").join(APP_NAME))
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
