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
/// working directory first (e2e builds only, where the test harness runs
/// from a scratch directory with a planted config), then — on macOS when
/// running as root — the console user's application config directory, then
/// the process's own platform-specific application config directory.
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

/// Return the search locations in priority order.
fn search_dirs() -> Vec<PathBuf> {
    ordered_search_dirs(
        cwd_path(),
        console_user_config_dir(),
        platform_config_dir(),
    )
}

/// Order the search locations, dropping duplicates while preserving
/// priority: the current working directory (e2e builds only), then the
/// console user's configuration directory (macOS, root only), then the
/// process's own platform configuration directory.
fn ordered_search_dirs(
    cwd: Option<PathBuf>,
    console_user_dir: Option<PathBuf>,
    platform_dir: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::<PathBuf>::new();
    for dir in [cwd, console_user_dir, platform_dir].iter().flatten() {
        if !dirs.contains(dir) {
            dirs.push(dir.clone());
        }
    }
    dirs
}

/// Return the configuration directory of the user currently at the console,
/// or `None` when it does not apply.
///
/// Only the root daemon needs this indirection: it runs with a home
/// directory of `/var/root`, while the configuration lives in the
/// logged-in user's home.  Unprivileged processes (the CLI, development
/// builds) already resolve their own home directory correctly.
#[cfg(target_os = "macos")]
fn console_user_config_dir() -> Option<PathBuf> {
    if unsafe { libc::geteuid() } != 0 {
        return None;
    }

    let home = crate::platform::console_user_home()?;
    Some(
        home.join("Library")
            .join("Application Support")
            .join(APP_NAME),
    )
}

/// Non-macOS platforms have no console-user indirection.
#[cfg(not(target_os = "macos"))]
fn console_user_config_dir() -> Option<PathBuf> {
    None
}

/// Return the current working directory, or `None` if it cannot be determined.
///
/// The CWD is only part of the search path in e2e builds, where the test
/// harness runs the daemon from a scratch directory with a planted config.
/// Production builds never search the CWD, so a daemon started from an
/// attacker-writable directory cannot load a planted `config.yaml`.
#[cfg(feature = "e2e")]
fn cwd_path() -> Option<PathBuf> {
    std::env::current_dir().ok()
}

/// The CWD is not searched in production builds (see the e2e variant).
#[cfg(not(feature = "e2e"))]
fn cwd_path() -> Option<PathBuf> {
    None
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
    for (i, dir) in search_dirs().into_iter().enumerate() {
        let label = if Some(dir.as_path()) == cwd.as_deref() {
            "Current working directory".to_string()
        } else {
            dir.display().to_string()
        };
        eprintln!("  {}. {label}", i + 1);
    }
}

// ---------------------------------------------------------------------------
// Config directory resolution
// ---------------------------------------------------------------------------

/// Return the platform-specific application config directory: the OS base
/// directory (resolved by [`crate::platform::config_dir`]) plus the
/// application name.  The directory may not exist yet.
fn platform_config_dir() -> Option<PathBuf> {
    crate::platform::config_dir().map(|d| d.join(APP_NAME))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// CWD is searched before the other locations in e2e builds.  All lookup
    /// functions and `print_search_locations` drive off this order.
    #[cfg(feature = "e2e")]
    #[test]
    fn search_dirs_prioritises_cwd_over_other_dirs() {
        let cwd = std::env::current_dir().unwrap();
        let dirs = search_dirs();

        assert_eq!(dirs.first(), Some(&cwd));
        if let Some(platform) = platform_config_dir() {
            assert!(dirs.contains(&platform));
        }
    }

    /// The CWD is not part of the search path in production builds, so a
    /// daemon started from an attacker-writable directory cannot load a
    /// planted `config.yaml`.
    #[cfg(not(feature = "e2e"))]
    #[test]
    fn search_dirs_excludes_cwd_in_production_builds() {
        let cwd = std::env::current_dir().unwrap();
        let dirs = search_dirs();

        assert!(!dirs.iter().any(|d| d == &cwd));
    }

    /// The search order is cwd, console-user dir, platform dir; duplicates
    /// are dropped while preserving the first (highest-priority) position.
    #[test]
    fn ordered_search_dirs_preserves_priority_and_dedupes() {
        let cwd = PathBuf::from("/tmp/e2e");
        let console = PathBuf::from(
            "/Users/alice/Library/Application Support/keymapperd",
        );
        let platform =
            PathBuf::from("/var/root/Library/Application Support/keymapperd");

        let dirs = ordered_search_dirs(
            Some(cwd.clone()),
            Some(console.clone()),
            Some(platform.clone()),
        );
        assert_eq!(dirs, vec![cwd.clone(), console, platform]);

        let dirs =
            ordered_search_dirs(Some(cwd.clone()), Some(cwd.clone()), None);
        assert_eq!(dirs, vec![cwd]);

        assert!(ordered_search_dirs(None, None, None).is_empty());
    }
}
