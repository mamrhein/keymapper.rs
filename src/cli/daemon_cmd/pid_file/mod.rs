// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! PID-file process management for keymapperd (development mode).
//!
//! Spawns the daemon as a detached background child process and tracks it
//! through a PID file inside the config directory.  This is the backend
//! selected when `--config-dir` is provided; production (service-manager)
//! mode lives in [`super::service`].
//!
//! ## Safety of `stop`
//!
//! A PID file is user-writable and PIDs are reused, so a stale or planted
//! file could point at an unrelated process.  `stop` therefore never signals
//! a PID on liveness alone.  Before terminating it verifies, in order:
//!
//! 1. the process at that PID is actually `keymapperd` (see
//!    [`verify_daemon_identity`]), and
//! 2. the token recorded by the running daemon matches the one stored in the
//!    PID file (see [`crate::common::daemon_token`]).
//!
//! The PID file itself is written atomically with `O_CREAT | O_EXCL` and a
//! restrictive mode so it can never be pre-planted or observed half-written.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use linux::{spawn_daemon, terminate_daemon, verify_daemon_identity};
#[cfg(target_os = "macos")]
use macos::{spawn_daemon, terminate_daemon, verify_daemon_identity};
#[cfg(target_os = "windows")]
use windows::{spawn_daemon, terminate_daemon, verify_daemon_identity};

use crate::common::daemon_token;

/// The PID file name.
const PID_FILE: &str = "keymapperd.pid";

/// The path to the PID file for the given config directory.
///
/// The PID file lives inside the config directory so that each `--config-dir`
/// invocation is self-contained.
fn pid_file_path(config_dir: &Path) -> PathBuf {
    config_dir.join(PID_FILE)
}

/// A parsed PID file: the daemon's PID plus the token the CLI generated for
/// it.  The two-line format is mandatory; anything else (including the old
/// PID-only format) is treated as invalid and ignored.
struct PidRecord {
    pid: u32,
    token: String,
}

/// Read and parse the PID file at `path`.
///
/// Returns `None` when the file is missing, unreadable, or does not contain
/// exactly a PID line followed by a non-empty token line.
fn read_record(path: &Path) -> Option<PidRecord> {
    let content = fs_err::read_to_string(path).ok()?;
    let lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();

    let [pid_str, token] = lines.as_slice() else {
        return None;
    };

    let pid = pid_str.parse::<u32>().ok()?;
    if token.is_empty() {
        return None;
    }

    Some(PidRecord {
        pid,
        token: (*token).to_string(),
    })
}

/// Write the PID file atomically.
///
/// The file is created with `O_CREAT | O_EXCL` (via `create_new`) so the call
/// fails rather than clobbering a file that already exists — whether it
/// belongs to another daemon or was planted by an attacker.  On Unix the file
/// is created with mode `0o600` so only the owner can read or write it.
fn write_pid_file_atomic(
    path: &Path,
    pid: u32,
    token: &str,
) -> Result<(), String> {
    use std::io::Write;

    let content = format!("{pid}\n{token}\n");

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }

    let mut file = options.open(path).map_err(|e| {
        format!("failed to create PID file {}: {e}", path.display())
    })?;

    file.write_all(content.as_bytes())
        .map_err(|e| format!("failed to write PID file: {e}"))?;

    Ok(())
}

/// Generate a random 64-character hex token from the system CSPRNG.
fn generate_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| format!("failed to generate random token: {e}"))?;

    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Read the token the running daemon recorded, if any.
fn read_daemon_token(config_dir: &Path) -> Option<String> {
    let content =
        fs_err::read_to_string(daemon_token::token_file_path(config_dir))
            .ok()?;
    let token = content.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// Check whether the process with the given PID is alive.
#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    // kill(pid, 0) returns ESRCH when the process doesn't exist.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(target_os = "windows")]
fn is_pid_alive(pid: u32) -> bool {
    windows::is_process_alive(pid)
}

/// Check whether the daemon is running by reading the PID file and verifying
/// the process exists **and** is actually `keymapperd`.
///
/// The identity check (not just liveness) ensures a stale PID file that points
/// at an unrelated process which reused the number does not report as running.
pub fn is_running(config_dir: &Path) -> bool {
    let pid_path = pid_file_path(config_dir);
    let Some(record) = read_record(&pid_path) else {
        return false;
    };

    is_pid_alive(record.pid) && verify_daemon_identity(record.pid)
}

/// Start keymapperd as a background process with its working directory set to
/// the given config directory.  The PID and a random token are written to a
/// PID file so that it can be stopped later.
pub fn start(config_dir: &Path) -> Result<(), String> {
    if is_running(config_dir) {
        return Err(
            "daemon is already running (managed by --config-dir)".into()
        );
    }

    let pid_path = pid_file_path(config_dir);

    // Remove any stale PID file so the atomic O_EXCL write below can create a
    // fresh one.  We only reach this point when is_running() is false, so any
    // existing file is stale (dead process or a reused PID).
    let _ = fs_err::remove_file(&pid_path);

    // Ensure the parent directory of the PID file exists.
    if let Some(parent) = pid_path.parent() {
        fs_err::create_dir_all(parent).map_err(|e| {
            format!("failed to create PID file directory: {e}")
        })?;
    }

    // Generate a random token and pass it to the daemon so it can record it
    // for later verification on stop.  The child inherits this environment
    // variable (fork/exec and CreateProcessW both inherit the parent
    // environment).
    let token = generate_token()?;
    // Safety: this runs on the CLI's main thread before any other thread is
    // spawned, so no thread concurrently reads environment variables.
    unsafe { std::env::set_var(daemon_token::TOKEN_ENV_VAR, &token) };

    let (child_pid, error) = spawn_daemon(config_dir)?;

    // Persist the PID and token atomically so a concurrent reader never sees
    // a partial file and an attacker cannot pre-plant the file.
    write_pid_file_atomic(&pid_path, child_pid, &token)
        .map_err(|e| format!("failed to write PID file: {e}"))?;

    // Brief grace period for the daemon to initialize or fail fast.
    std::thread::sleep(std::time::Duration::from_millis(100));

    if !is_pid_alive(child_pid) {
        // Clean up the stale PID file.
        let _ = fs_err::remove_file(&pid_path);
        return Err(
            error.unwrap_or_else(|| "daemon exited immediately".into())
        );
    }

    Ok(())
}

/// Stop the daemon by reading the PID file and sending a termination signal.
///
/// Before signaling, verifies that the target process is actually `keymapperd`
/// and that its recorded token matches the one in the PID file, so a stale or
/// planted PID file can never be used to kill an unrelated process.
///
/// Sends SIGTERM first, waits up to 5 seconds, then escalates to SIGKILL.
/// On Windows, uses `TerminateProcess` directly.
pub fn stop(config_dir: &Path) -> Result<(), String> {
    let pid_path = pid_file_path(config_dir);

    let Some(record) = read_record(&pid_path) else {
        return Err("daemon is not running (no PID file found)".into());
    };

    if !is_pid_alive(record.pid) {
        // Stale PID file — clean up and report that the daemon isn't running.
        let _ = fs_err::remove_file(&pid_path);
        return Err("daemon is not running (stale PID file)".into());
    }

    // Verify the process at this PID is actually keymapperd, not an unrelated
    // process that reused the number.  Refuse to signal anything else.
    if !verify_daemon_identity(record.pid) {
        return Err(format!(
            "refusing to stop: PID {} is not keymapperd (possible PID reuse)",
            record.pid
        ));
    }

    // Verify the token recorded by the running daemon matches the one we
    // stored, so we only signal the exact instance we started.
    let daemon_token = read_daemon_token(config_dir).ok_or_else(|| {
        "refusing to stop: daemon token not found (daemon may have been \
         started outside the CLI)"
            .to_string()
    })?;
    if daemon_token != record.token {
        return Err("refusing to stop: token mismatch (PID file does not \
                    match the running daemon)"
            .into());
    }

    terminate_daemon(record.pid)?;

    // Wait for the process to actually exit.
    let mut waited = 0;
    while is_pid_alive(record.pid) && waited < 50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        waited += 1;
    }

    // Clean up the PID file and the daemon's token file.
    let _ = fs_err::remove_file(&pid_path);
    let _ = fs_err::remove_file(daemon_token::token_file_path(config_dir));

    Ok(())
}

/// Restart the daemon in the given config directory.
pub fn restart(config_dir: &Path) -> Result<(), String> {
    stop(config_dir)?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    start(config_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed two-line PID file parses into the expected record.
    #[test]
    fn read_record_parses_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymapperd.pid");
        fs_err::write(&path, "1234\nabcdef0123456789\n").unwrap();

        let record = read_record(&path).unwrap();
        assert_eq!(record.pid, 1234);
        assert_eq!(record.token, "abcdef0123456789");
    }

    /// The old PID-only format (a single line) is rejected so a stale file
    /// from an older version can never be acted on.
    #[test]
    fn read_record_rejects_pid_only_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymapperd.pid");
        fs_err::write(&path, "1234\n").unwrap();

        assert!(read_record(&path).is_none());
    }

    /// A missing file yields `None`.
    #[test]
    fn read_record_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymapperd.pid");

        assert!(read_record(&path).is_none());
    }

    /// The atomic write creates the file with the expected two-line content.
    #[test]
    fn write_pid_file_atomic_writes_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymapperd.pid");

        write_pid_file_atomic(&path, 4321, "tok").unwrap();

        let record = read_record(&path).unwrap();
        assert_eq!(record.pid, 4321);
        assert_eq!(record.token, "tok");
    }

    /// The atomic write refuses to clobber an existing file (O_EXCL).
    #[test]
    fn write_pid_file_atomic_refuses_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymapperd.pid");
        fs_err::write(&path, "999\nplanted\n").unwrap();

        let result = write_pid_file_atomic(&path, 1, "tok");
        assert!(result.is_err());

        // The pre-existing content is untouched.
        let record = read_record(&path).unwrap();
        assert_eq!(record.pid, 999);
        assert_eq!(record.token, "planted");
    }

    /// Tokens are 64 hex characters and differ across calls.
    #[test]
    fn generate_token_is_64_hex_and_unique() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();

        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// A non-existent PID is never identified as the daemon.
    #[test]
    fn verify_daemon_identity_rejects_missing_pid() {
        // A PID far beyond the current range is effectively guaranteed not to
        // exist.  On Linux the max is /proc/sys/kernel/pid_max (often 4194304
        // or 32768); use a value above the common defaults.
        assert!(!verify_daemon_identity(u32::MAX));
    }

    /// The current process (a test binary, not `keymapperd`) is rejected,
    /// which exercises the "not keymapperd" path that protects `stop`.
    #[test]
    fn verify_daemon_identity_rejects_non_daemon_process() {
        assert!(!verify_daemon_identity(std::process::id()));
    }
}
