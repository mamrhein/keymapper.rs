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
/// Searches the locations from [`search_dirs`] in priority order: current
/// working directory first (development convenience), then the
/// platform-specific application config directory.
///
/// Symbolic links are rejected; `config.yaml` must be a regular file.
/// Returns `None` when no configuration file exists in any search location.
pub fn find_config_path() -> Option<PathBuf> {
    for dir in search_dirs() {
        let path = dir.join(CONFIG_FILE);
        if path.is_file() && !is_symlink(&path) {
            return Some(path);
        }
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
    [cwd_path()]
        .into_iter()
        .flatten()
        .chain(platform_config_dir())
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

    // Drive off `search_dirs()` so the printed order can never drift from
    // the actual search order.
    let cwd = cwd_path();
    for (i, dir) in search_dirs().enumerate() {
        let label = if Some(dir.as_path()) == cwd.as_deref() {
            "Current working directory".to_string()
        } else {
            dir.display().to_string()
        };
        eprintln!("  {}. {label}", i + 1);
    }
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
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .map(|d| d.join(APP_NAME))
}

#[cfg(target_os = "windows")]
fn platform_config_dir() -> Option<PathBuf> {
    // %APPDATA%\keymapperd
    dirs::config_dir().map(|d| d.join(APP_NAME))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// CWD is searched before the platform config directory.  All lookup
    /// functions and `print_search_locations` drive off this order.
    #[test]
    fn search_dirs_prioritises_cwd_over_platform_dir() {
        let cwd = std::env::current_dir().unwrap();
        let dirs: Vec<PathBuf> = search_dirs().collect();

        assert_eq!(dirs.first(), Some(&cwd));
        if let Some(platform) = platform_config_dir() {
            assert_eq!(dirs.len(), 2);
            assert_eq!(dirs[1], platform);
        } else {
            assert_eq!(dirs.len(), 1);
        }
    }
}
