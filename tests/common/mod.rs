// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Shared helpers for the integration test crates.
//!
//! Each integration test is its own crate, so helpers used by more than one
//! test crate live here and are pulled in via `mod common;`.

use std::{
    env, thread,
    time::{Duration, Instant},
};

/// Cross-process lock that serializes tests touching the global input stack
/// or daemon process state.
///
/// The e2e tests drive the system-wide input stack (they grab the physical
/// keyboard and create virtual devices), so two must never run
/// concurrently: a second daemon would install a session-wide keyboard hook
/// that swallows the other test's events.  `server_start_not_found`
/// likewise probes the process list for `keymapperd`, which a concurrent
/// e2e daemon would pollute.  Holding the lock for the duration of the test
/// serializes those tests while leaving the unit tests free to run in
/// parallel.
///
/// The lock must work across processes because nextest runs every test in
/// its own process; an in-process `std::sync::Mutex` would be useless.
#[cfg(unix)]
pub struct E2eLock {
    /// The lock is released when this file (and its descriptor) is closed.
    _file: std::fs::File,
}

#[cfg(unix)]
impl E2eLock {
    /// Block until the exclusive e2e lock is acquired.
    pub fn acquire() -> Self {
        use std::os::unix::io::AsRawFd;

        let path = env::temp_dir().join("keymapper_e2e.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .unwrap_or_else(|e| {
                panic!("failed to open e2e lock file {path:?}: {e}")
            });

        // Safety: flock(2) on a descriptor owned by `file`.
        loop {
            let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if ret == 0 {
                return E2eLock { _file: file };
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                panic!("failed to acquire e2e lock {path:?}: {err}");
            }
        }
    }
}

/// Windows implementation of the cross-process e2e lock.
///
/// Windows has no `flock`, so a file opened without any share mode acts as
/// the lock: while the holder keeps it open, every other `CreateFileW`
/// fails with `ERROR_SHARING_VIOLATION`, and the kernel closes it when the
/// process dies, so a crashed or killed test cannot leave the lock held.
/// The `HANDLE` closes itself on drop, releasing the lock.
#[cfg(not(unix))]
pub struct E2eLock {
    /// The lock is released when this handle is closed (on drop or exit).
    _file: windows::Win32::Foundation::HANDLE,
}

#[cfg(not(unix))]
impl E2eLock {
    /// Block until the exclusive e2e lock is acquired.
    pub fn acquire() -> Self {
        use std::os::windows::ffi::OsStrExt;

        use windows::{
            Win32::{
                Foundation::{
                    ERROR_SHARING_VIOLATION, GENERIC_READ, GENERIC_WRITE,
                },
                Storage::FileSystem::{
                    CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL,
                    FILE_SHARE_MODE,
                },
            },
            core::{HRESULT, PCWSTR},
        };

        let path = env::temp_dir().join("keymapper_e2e.lock");
        // `name` borrows from `wide`, so both must outlive the retry loop.
        let wide: Vec<u16> =
            path.as_os_str().encode_wide().chain(Some(0)).collect();
        let name = PCWSTR::from_raw(wide.as_ptr());
        let sharing_violation = HRESULT::from_win32(ERROR_SHARING_VIOLATION.0);

        // Retry on `ERROR_SHARING_VIOLATION` until a concurrent test
        // releases the lock (up to 10 minutes; the tests take at most a
        // few dozen seconds).
        let deadline = Instant::now() + Duration::from_secs(10 * 60);
        loop {
            match unsafe {
                CreateFileW(
                    name,
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_MODE(0),
                    None,
                    CREATE_ALWAYS,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            } {
                Ok(file) => return E2eLock { _file: file },
                Err(e) if e.code() == sharing_violation => {
                    if Instant::now() >= deadline {
                        panic!("timed out waiting for the e2e lock {path:?}");
                    }
                    thread::sleep(Duration::from_millis(200));
                }
                Err(e) => panic!("failed to open e2e lock file {path:?}: {e}"),
            }
        }
    }
}
