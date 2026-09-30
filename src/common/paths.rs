// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The single source of truth for the per-user data-directory layout.
//!
//! The config file, the daemon log, and (on Windows) the control-pipe publish
//! file all live under a `<base>/<APP_DIR_NAME>` directory, where `<base>` is
//! the OS-specific root.  This module owns the directory *name* and the
//! file-name conventions so the three consumers — [`config_path`]
//! ([`crate::common::config_path`]), the daemon logger
//! ([`crate::daemon::logging`]), and the Windows control endpoint
//! ([`crate::daemon::control`]) — agree on the layout instead of each
//! hardcoding a copy (which is how the macOS log directory drifted to
//! `keymapper` while everything else used `keymapperd`).

// Only the non-Linux helpers build a `PathBuf`; Linux logs to stderr.
#[cfg(not(target_os = "linux"))]
use std::path::PathBuf;

/// The application directory name appended to each OS base directory.
///
/// Standardized to `keymapperd` across config, log, and pipe locations; the
/// CLI binary is `keymapper`, but the on-disk data directory has always been
/// the daemon's name.
pub(crate) const APP_DIR_NAME: &str = "keymapperd";

/// The configuration file name inside the config directory.
pub(crate) const CONFIG_FILE_NAME: &str = "config.yaml";

/// The log subdirectory under [`local_app_data_dir`] (Windows).
#[cfg(windows)]
pub(crate) const LOG_DIR_NAME: &str = "logs";

/// The file that publishes the daemon's live control-pipe name to the CLI
/// (Windows).
#[cfg(windows)]
pub(crate) const CONTROL_PUBLISH_FILE_NAME: &str = "control.pipe";

/// The daemon's directory under the per-user local app-data root
/// (`%LOCALAPPDATA%\keymapperd`), shared by the log directory and the
/// control-pipe publish file so both sides resolve the same location.
#[cfg(windows)]
pub(crate) fn local_app_data_dir() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join(APP_DIR_NAME))
}

/// The log directory on the file-logging platforms (macOS, Windows).
///
/// Linux logs to stderr (the journal forwards it), so this is not compiled
/// there.  Returns a descriptive error when the OS base directory cannot be
/// resolved.
#[cfg(not(target_os = "linux"))]
pub(crate) fn log_dir() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        local_app_data_dir()
            .map(|dir| dir.join(LOG_DIR_NAME))
            .ok_or_else(|| {
                "no local data directory (LOCALAPPDATA) available".to_string()
            })
    }
    #[cfg(not(windows))]
    {
        // macOS: ~/Library/Logs/<APP_DIR_NAME>
        dirs::home_dir()
            .map(|home| home.join("Library").join("Logs").join(APP_DIR_NAME))
            .ok_or_else(|| "no home directory available".to_string())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_dir_name_is_the_daemon_name() {
        assert_eq!(APP_DIR_NAME, "keymapperd");
    }

    #[test]
    fn config_file_name_is_config_yaml() {
        assert_eq!(CONFIG_FILE_NAME, "config.yaml");
    }
}
