// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS per-user configuration base directory resolution.

use std::path::PathBuf;

/// Return the OS-specific per-user configuration base directory, without the
/// application name.
///
/// On macOS this is `~/Library/Application Support`.  The directory may not
/// exist yet.
pub fn config_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library").join("Application Support"))
}

/// Return the uid of the user currently at the console.
///
/// The owner of `/dev/console` is the console user.  Returns `None` when
/// there is no console user (headless system, or the login window before any
/// user has logged in).
///
/// The virtkbdd daemon uses this to verify the peer of an IPC connection and
/// to `chown` its socket, because it runs as root while the console user's
/// keymapperd connects to it.
pub fn console_uid() -> Option<libc::uid_t> {
    let console = std::ffi::CString::new("/dev/console").ok()?;
    let fd = unsafe { libc::open(console.as_ptr(), libc::O_RDONLY) };
    if fd < 0 {
        return None;
    }

    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    let status = unsafe { libc::fstat(fd, &mut stat) };
    // The descriptor is only needed for the metadata lookup.
    unsafe { libc::close(fd) };
    if status != 0 {
        return None;
    }

    Some(stat.st_uid)
}

/// Return the home directory of the user currently at the console.
///
/// The owner of `/dev/console` is the console user.  A process running as
/// root uses this to locate the configuration of the logged-in user, because
/// its own home directory is `/var/root`.  The config path search in
/// `common::config_path` consults it when the effective UID is 0.
///
/// Returns `None` when there is no console user (headless system, or the
/// login window before any user has logged in) and when the console user is
/// root (single-user mode), in which case [`config_dir`] already points at
/// the right place.
pub fn console_user_home() -> Option<PathBuf> {
    let uid = console_uid()?;

    // No logged-in user yet, or a root console: the regular config dir is
    // already the right search location in both cases.
    if uid == 0 {
        return None;
    }

    passwd_home(uid)
}

/// Look up the home directory of *uid* in the password database.
fn passwd_home(uid: libc::uid_t) -> Option<PathBuf> {
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    // 1024 bytes covers any realistic pw_name/pw_dir; getpwuid_r reports
    // ERANGE when the buffer is too small.
    let mut buffer = [0u8; 1024];
    let mut result: *mut libc::passwd = std::ptr::null_mut();

    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return None;
    }

    let home = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
    home.to_str().ok().map(PathBuf::from)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Root's home directory resolves through the password database.
    #[test]
    fn passwd_home_resolves_root() {
        let home = passwd_home(0).expect("root entry must exist");
        assert!(home.is_absolute());
    }

    /// A uid without a password entry yields `None`.
    #[test]
    fn passwd_home_missing_uid() {
        // u32::MAX is never assigned to an account.
        assert!(passwd_home(u32::MAX).is_none());
    }

    /// The console uid is either absent (headless environment) or a valid,
    /// non-negative uid.
    #[test]
    fn console_uid_is_sane() {
        // On a headless CI runner there may be no console user; both outcomes
        // are acceptable, so only assert the value is well-formed when
        // present.
        if let Some(uid) = console_uid() {
            assert!(uid != libc::uid_t::MAX);
        }
    }

    /// The console user lookup either fails cleanly (headless environment or
    /// root console) or returns an absolute home directory that is not
    /// root's.
    #[test]
    fn console_user_home_is_sane() {
        let Some(home) = console_user_home() else {
            return;
        };

        assert!(home.is_absolute());
        assert_ne!(home, PathBuf::from("/var/root"));
    }
}
