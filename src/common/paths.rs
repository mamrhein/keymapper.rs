// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The single source of truth for the per-user data-directory layout
//! *names*.
//!
//! The config file, the daemon log, and (on Windows) the control-pipe
//! publish file all live under a `<base>/<APP_DIR_NAME>` directory, where
//! `<base>` is the OS-specific root resolved by [`crate::platform`].
//! This module owns the directory *name* and the file-name conventions —
//! pure strings, no OS-specific resolution — so the consumers —
//! [`config_path`] ([`crate::common::config_path`]), the platform log
//! destination ([`crate::platform::logging`]), and the Windows control
//! endpoint ([`crate::platform::endpoint`]) — agree on the layout instead
//! of each hardcoding a copy (which is how the macOS log directory drifted
//! to `keymapper` while everything else used `keymapperd`).

/// The application directory name appended to each OS base directory.
///
/// Standardized to `keymapperd` across config, log, and pipe locations; the
/// CLI binary is `keymapper`, but the on-disk data directory has always been
/// the daemon's name.
pub(crate) const APP_DIR_NAME: &str = "keymapperd";

/// The configuration file name inside the config directory.
pub(crate) const CONFIG_FILE_NAME: &str = "config.yaml";

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
