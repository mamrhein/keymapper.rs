// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Shared constants and helpers for the PID-file token mechanism.
//!
//! In development (PID-file) mode the CLI generates a random token, passes it
//! to the daemon through an environment variable, and stores it alongside the
//! PID in the PID file.  The daemon records the same token in a separate file
//! so that `stop` can confirm it is signaling the exact daemon instance it
//! started, rather than an unrelated process that happened to reuse the PID.
//!
//! Both the CLI and the daemon link against this module, so the environment
//! variable name and the token file name can never drift apart.

use std::path::{Path, PathBuf};

/// Environment variable carrying the token from the CLI to the daemon.
pub const TOKEN_ENV_VAR: &str = "KEYMAPPER_PID_TOKEN";

/// File name (inside the config directory) where the daemon records its token.
pub const TOKEN_FILE: &str = "keymapperd.token";

/// The path to the token file for the given config directory.
pub fn token_file_path(config_dir: &Path) -> PathBuf {
    config_dir.join(TOKEN_FILE)
}

/// Record the token passed via [`TOKEN_ENV_VAR`] into the config directory.
///
/// This is called by the daemon at startup.  When the environment variable is
/// not set (production / service mode) or is empty, this is a no-op, so the
/// daemon never writes a token file outside of PID-file development mode.
///
/// A failure to write the file is intentionally non-fatal: it only means that
/// `stop` will later refuse to signal this instance, which is the safe
/// outcome.
pub fn record_token(config_dir: &Path) {
    let Ok(token) = std::env::var(TOKEN_ENV_VAR) else {
        return;
    };
    if token.is_empty() {
        return;
    }

    let _ = fs_err::write(token_file_path(config_dir), format!("{token}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `record_token` writes the token to the expected file when the
    /// environment variable is set.
    #[test]
    fn record_token_writes_file_when_env_set() {
        let dir = tempfile::tempdir().unwrap();

        // Safety: single-threaded test; no other thread reads this variable.
        unsafe { std::env::set_var(TOKEN_ENV_VAR, "test-token-123") };
        record_token(dir.path());
        unsafe { std::env::remove_var(TOKEN_ENV_VAR) };

        let content =
            fs_err::read_to_string(token_file_path(dir.path())).unwrap();
        assert_eq!(content.trim(), "test-token-123");
    }

    /// `record_token` is a no-op when the environment variable is unset, so
    /// production (service) mode never creates a token file.
    #[test]
    fn record_token_is_noop_when_env_unset() {
        let dir = tempfile::tempdir().unwrap();

        // Safety: single-threaded test; no other thread reads this variable.
        unsafe { std::env::remove_var(TOKEN_ENV_VAR) };
        record_token(dir.path());

        assert!(!token_file_path(dir.path()).exists());
    }
}
