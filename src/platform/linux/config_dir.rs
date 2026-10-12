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
/// Uses `$XDG_CONFIG_HOME` when it is set and absolute, otherwise falls back
/// to `~/.config`. A relative or empty value is rejected — it would silently
/// re-root config and socket resolution into the CWD, where an attacker-
/// writable directory passes the parent-chain ownership check and could serve
/// a planted `config.yaml`.
pub fn config_dir() -> Option<PathBuf> {
    match std::env::var("XDG_CONFIG_HOME") {
        Ok(v) if !v.is_empty() && PathBuf::from(&v).is_absolute() => {
            Some(PathBuf::from(v))
        }
        _ => dirs::home_dir().map(|h| h.join(".config")),
    }
}

/// Linux has no console-user indirection.
pub(crate) fn console_user_config_dir() -> Option<PathBuf> {
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A relative `XDG_CONFIG_HOME` is rejected so it cannot re-root config
    /// resolution into the CWD (R3-9).
    #[test]
    fn config_dir_rejects_relative_xdg_config_home() {
        std::env::set_var("XDG_CONFIG_HOME", "relative/dir");
        let dir = config_dir();
        assert!(dir.is_some());
        assert!(dir.unwrap().is_absolute());
    }

    /// An empty `XDG_CONFIG_HOME` is rejected (R3-9).
    #[test]
    fn config_dir_rejects_empty_xdg_config_home() {
        std::env::set_var("XDG_CONFIG_HOME", "");
        let dir = config_dir();
        assert!(dir.is_some());
        assert!(dir.unwrap().is_absolute());
    }

    /// An absolute `XDG_CONFIG_HOME` is used directly (R3-9 happy path).
    #[test]
    fn config_dir_uses_absolute_xdg_config_home() {
        std::env::set_var("XDG_CONFIG_HOME", "/etc/config-home");
        let dir = config_dir();
        assert_eq!(dir, Some(PathBuf::from("/etc/config-home")));
    }
}
