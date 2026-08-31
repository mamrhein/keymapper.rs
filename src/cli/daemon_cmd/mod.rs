// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Daemon process management for keymapperd.
//!
//! A single process-manager exposes `status` / `start` / `stop` / `restart`
//! on top of two interchangeable backends:
//!
//! * [`Backend::ServiceManager`] — production mode.  Delegates to the platform
//!   service manager (launchd / `systemctl --user`) or spawns the binary
//!   directly on Windows.  Selected when no `--config-dir` is given.
//! * [`Backend::PidFile`] — development mode.  Spawns the daemon as a detached
//!   background child and tracks it through a PID file.  Selected when
//!   `--config-dir` is given.
//!
//! The backend is chosen **once per invocation** by
//! [`Backend::from_config_dir`], so all four operations always act on the
//! same mechanism rather than drifting between them.

mod pid_file;
mod service;

use std::path::PathBuf;

/// The backend used to manage the keymapperd process.
pub enum Backend {
    /// Production mode: the platform service manager (launchd /
    /// `systemctl --user`) or a direct spawn on Windows.
    ServiceManager,

    /// Development mode: a detached background process tracked through a PID
    /// file.  Holds the directory containing `config.yaml` and the PID file.
    PidFile(PathBuf),
}

impl Backend {
    /// Choose the backend from an optional `--config-dir` value.
    ///
    /// A provided directory selects PID-file management; its absence selects
    /// the platform service manager.
    pub fn from_config_dir(config_dir: Option<PathBuf>) -> Self {
        match config_dir {
            Some(dir) => Backend::PidFile(dir),
            None => Backend::ServiceManager,
        }
    }
}

/// Check whether keymapperd is running under the given backend.
pub fn is_running(backend: &Backend) -> bool {
    match backend {
        Backend::ServiceManager => service::is_running(),
        Backend::PidFile(dir) => pid_file::is_running(dir),
    }
}

/// Start keymapperd under the given backend.
pub fn start(backend: &Backend) -> Result<(), String> {
    match backend {
        Backend::ServiceManager => service::start(),
        Backend::PidFile(dir) => pid_file::start(dir),
    }
}

/// Stop keymapperd under the given backend.
pub fn stop(backend: &Backend) -> Result<(), String> {
    match backend {
        Backend::ServiceManager => service::stop(),
        Backend::PidFile(dir) => pid_file::stop(dir),
    }
}

/// Restart keymapperd under the given backend.
pub fn restart(backend: &Backend) -> Result<(), String> {
    match backend {
        Backend::ServiceManager => service::restart(),
        Backend::PidFile(dir) => pid_file::restart(dir),
    }
}
