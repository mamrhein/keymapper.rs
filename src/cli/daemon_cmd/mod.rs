// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Daemon process-management commands (`keymapper daemon …`).
//!
//! The platform service manager (launchd / `systemctl --user`, or a direct
//! spawn on Windows) is the sole owner of the daemon's lifecycle; the raw
//! operations are provided by the platform-specific modules below (kept
//! private).  The `pub` functions here add the user-facing reporting and
//! the runtime log-level control on top, so the binary only has to parse
//! arguments and dispatch.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

// `is_running` stays public (the e2e harness probes it). The lifecycle
// operations are aliased so the command functions below can share the verb
// names (`start`, `stop`, `restart`) without colliding with them.
#[cfg(target_os = "linux")]
pub use linux::is_running;
#[cfg(target_os = "linux")]
use linux::{
    restart as restart_service, start as start_service, stop as stop_service,
};
#[cfg(target_os = "macos")]
pub use macos::is_running;
#[cfg(target_os = "macos")]
use macos::{
    restart as restart_service, start as start_service, stop as stop_service,
    virtkbdd_is_running, virtkbdd_restart, virtkbdd_start, virtkbdd_stop,
};
#[cfg(target_os = "windows")]
pub use windows::is_running;
#[cfg(target_os = "windows")]
use windows::{
    restart as restart_service, start as start_service, stop as stop_service,
};

use crate::daemon::{control, logging::LevelFilter};

/// Report whether keymapperd (and, on macOS, virtkbdd) is running.
pub fn status() {
    if is_running() {
        println!("keymapperd is running");
    } else {
        println!("keymapperd is not running");
    }

    // On macOS the service manager also owns virtkbdd; report it as well.
    #[cfg(target_os = "macos")]
    if virtkbdd_is_running() {
        println!("virtkbdd is running");
    } else {
        println!("virtkbdd is not running");
    }
}

/// Start keymapperd if it is not already running.
pub fn start() -> Result<(), Box<dyn std::error::Error>> {
    if is_running() {
        println!("keymapperd is already running");
    } else {
        start_service()?;
        println!("keymapperd started");
    }

    // On macOS the service manager also owns virtkbdd; bring it up even when
    // keymapperd was already running.
    #[cfg(target_os = "macos")]
    if !virtkbdd_is_running() {
        virtkbdd_start()?;
        println!("virtkbdd started");
    }

    Ok(())
}

/// Stop keymapperd if it is running.
pub fn stop() -> Result<(), Box<dyn std::error::Error>> {
    if !is_running() {
        println!("keymapperd is not running");
    } else {
        stop_service()?;
        println!("keymapperd stopped");
    }

    // On macOS the service manager also owns virtkbdd; stop it even when
    // keymapperd was not running.
    #[cfg(target_os = "macos")]
    if virtkbdd_is_running() {
        virtkbdd_stop()?;
        println!("virtkbdd stopped");
    }

    Ok(())
}

/// Restart keymapperd (stop then start).
pub fn restart() -> Result<(), Box<dyn std::error::Error>> {
    restart_service()?;
    println!("keymapperd restarted");

    // On macOS the service manager also owns virtkbdd.
    #[cfg(target_os = "macos")]
    {
        virtkbdd_restart()?;
        println!("virtkbdd restarted");
    }

    Ok(())
}

/// Ask the running daemon to change its log level, over its control socket.
///
/// The daemon replies `OK <level>` on success. A connection failure means no
/// daemon is reachable (not running, or older than this CLI); a daemon reply
/// starting with `ERROR` means the request was rejected.
///
/// At `debug` and above the daemon records every key-down with its resolved
/// HID usage, enough to reconstruct typed input, so after confirming the new
/// level a warning is printed telling the user not to type sensitive data
/// while it is active.
pub fn log(level: LevelFilter) -> Result<(), Box<dyn std::error::Error>> {
    let reply = match control::set_log_level(level) {
        Ok(reply) => reply,
        Err(control::ControlError::Connect(reason)) => {
            return Err(format!(
                "{reason}. Is keymapperd running, and at least as new as \
                 this CLI?"
            )
            .into());
        }
        Err(e) => return Err(e.to_string().into()),
    };

    if let Some(reason) = reply.strip_prefix("ERROR ") {
        return Err(reason.into());
    }
    println!("{reply}");
    if level >= LevelFilter::Debug {
        eprintln!(
            "Warning: keystrokes are written to the daemon log at the \
             {level:?} level. Do not type sensitive data (passwords, keys, \
             tokens) while it is active!"
        );
    }
    Ok(())
}
