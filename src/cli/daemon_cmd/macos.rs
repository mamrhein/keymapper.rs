// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS-specific daemon spawning and termination via `fork` / `exec`.
//!
//! Uses a double-fork pattern to daemonize the child process, detaching it
//! from the terminal session.

use std::path::Path;

/// Resolve the path to the `keymapperd` binary as a `CString`.
///
/// Prefers the binary located next to this CLI executable so that a
/// development build never silently falls back to a stale `keymapperd`
/// installed on `PATH`.  Returns `None` when no sibling binary exists;
/// callers then fall back to a plain `PATH` lookup.
fn resolve_daemon_binary() -> Option<std::ffi::CString> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("keymapperd")))
        .filter(|path| path.is_file())
        .and_then(|path| {
            std::ffi::CString::new(path.to_string_lossy().into_owned()).ok()
        })
}

/// Spawn `keymapperd` as a background child process with the given current
/// directory.  Returns the child PID and an optional error captured from the
/// daemon's stderr (if available before it detaches).
///
/// The daemon's stdout and stderr are redirected to
/// `<config_dir>/keymapperd.log` so that startup failures (e.g. a missing
/// DriverKit driver) are captured for debugging instead of being discarded.
pub fn spawn_daemon(
    config_dir: &Path,
) -> Result<(u32, Option<String>), String> {
    // Safety: fork is a simple libc call that duplicates the current process.
    // The child executes exec to replace its image with keymapperd.
    let pid = unsafe { libc::fork() };

    match pid {
        -1 => Err(format!("fork failed: {}", std::io::Error::last_os_error())),
        0 => {
            // Child process — detach from the parent session.
            // Safety: setsid creates a new session, making this process a
            // session leader and detaching it from the controlling terminal.
            unsafe {
                libc::setsid();

                // Redirect stdin to /dev/null so the daemon doesn't hold
                // terminal input resources.
                let dev_null =
                    libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
                if dev_null >= 0 {
                    libc::dup2(dev_null, libc::STDIN_FILENO);
                    if dev_null > 2 {
                        libc::close(dev_null);
                    }
                }

                // Redirect stdout/stderr to a log file in the config directory
                // so startup failures are captured for debugging.  Opened with
                // an absolute path before the chdir below.
                let log_path = config_dir.join("keymapperd.log");
                let log_cstr = std::ffi::CString::new(
                    log_path.to_string_lossy().into_owned(),
                )
                .unwrap_or_default();
                let log_fd = libc::open(
                    log_cstr.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                    0o644,
                );
                if log_fd >= 0 {
                    libc::dup2(log_fd, libc::STDOUT_FILENO);
                    libc::dup2(log_fd, libc::STDERR_FILENO);
                    if log_fd > 2 {
                        libc::close(log_fd);
                    }
                }

                // Set the working directory so the daemon finds its config.
                let dir_str = config_dir.to_string_lossy();
                let c_dir = std::ffi::CString::new(dir_str.as_ref())
                    .unwrap_or_default();
                libc::chdir(c_dir.as_ptr());

                // Replace this process with the keymapperd binary.
                match resolve_daemon_binary() {
                    Some(c_exe) => {
                        libc::execvp(c_exe.as_ptr(), std::ptr::null());
                    }
                    None => {
                        let exe = b"keymapperd\0";
                        libc::execvp(
                            exe.as_ptr() as *const i8,
                            std::ptr::null(),
                        );
                    }
                }

                // execvp returned — it failed.  Exit the child gracefully.
                std::process::exit(1);
            }
        }
        _ => {
            // Parent process — `pid` is the child's PID.
            Ok((pid as u32, None))
        }
    }
}

/// Terminate the daemon process.  Sends SIGTERM first, then escalates to
/// SIGKILL after a brief delay if the process is still alive.
pub fn terminate_daemon(pid: u32) -> Result<(), String> {
    // Send SIGTERM for graceful shutdown.
    let ret = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if ret != 0 {
        return Err(format!(
            "failed to send SIGTERM: {}",
            std::io::Error::last_os_error()
        ));
    }

    // Wait up to 3 seconds for graceful shutdown.
    let mut waited = 0;
    while waited < 30 {
        if unsafe { libc::kill(pid as i32, 0) != 0 } {
            // Process is gone (ESRCH returns -1 with errno set).
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        waited += 1;
    }

    // Process still alive — force kill.
    if unsafe { libc::kill(pid as i32, 0) == 0 } {
        let ret = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        if ret != 0 {
            return Err(format!(
                "failed to send SIGKILL: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    Ok(())
}
