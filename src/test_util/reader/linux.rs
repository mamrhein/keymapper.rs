// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux reader setup: become the foreground process of the active virtual
//! terminal (tty1) so the daemon's uinput output — which the console driver
//! routes to the active VT — is delivered to this process's stdin.
//!
//! The harness stops `getty@tty1` and runs `chvt 1` before spawning the
//! reader, so tty1 is active and unowned.  The reader then claims it: a new
//! session leader that opens the tty acquires it as its controlling terminal
//! and becomes the tty's foreground process group.  An *inherited* open fd
//! would not do this (the kernel only assigns the ctty when the session
//! leader itself opens the tty), so `setsid()` and the `open()` must both
//! happen in this process.

/// The virtual terminal the harness activates and the reader claims.
const TTY: &str = "/dev/tty1";

/// Acquire tty1 as the controlling terminal and put it in raw mode.
///
/// On success, stdin (fd 0) is tty1 and each keystroke is available
/// immediately (no line buffering, no echo).  Exits the process on failure,
/// since a reader that cannot claim the terminal is useless to the harness.
pub(super) fn setup() {
    // Become a session leader so the tty open below can assign us a
    // controlling terminal.  The reader is spawned by the harness (not a
    // process-group leader), so this succeeds.
    // Safety: setsid(2) takes no arguments and only affects this process.
    if unsafe { libc::setsid() } == -1 {
        eprintln!("error: setsid failed: {}", std::io::Error::last_os_error());
        std::process::exit(1);
    }

    // Open the active VT.  As the new session leader, this assigns tty1 as
    // our controlling terminal and makes our process group its foreground
    // group, so keyboard input is delivered to us.
    // Safety: open(2) on a fixed device path; the returned fd is owned by us.
    let tty_fd = unsafe {
        libc::open(std::ffi::CString::new(TTY).unwrap().as_ptr(), libc::O_RDWR)
    };
    if tty_fd == -1 {
        eprintln!(
            "error: failed to open {TTY}: {}",
            std::io::Error::last_os_error()
        );
        std::process::exit(1);
    }

    // Make the tty our stdin so the shared read loop can use std::io::stdin.
    // Safety: dup2(2) on fds we own; overwriting fd 0 is intentional.
    if unsafe { libc::dup2(tty_fd, 0) } == -1 {
        eprintln!(
            "error: dup2 to stdin failed: {}",
            std::io::Error::last_os_error()
        );
        std::process::exit(1);
    }

    // The tty fd is now redundant (stdin aliases it); close the extra copy.
    // Safety: close(2) on a fd we own.
    unsafe { libc::close(tty_fd) };

    super::make_raw(0);
}
