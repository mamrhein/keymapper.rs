// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Raw epoll FFI and fd lifecycle for cross-thread multiplexing.
//!
//! The nix `Epoll` type owns the epoll fd but is not `Sync`, preventing safe
//! sharing across threads. Because epoll_ctl(2) and epoll_wait(2) are safe to
//! call concurrently on the same epoll fd, this module uses thin libc
//! wrappers for every epoll operation. That avoids the nix `Epoll` type
//! entirely and gives full control over cross-thread access.

use std::os::unix::io::RawFd;

use libc::{c_int, epoll_event, epoll_wait as libc_epoll_wait};

unsafe extern "C" {
    fn epoll_create1(flags: c_int) -> c_int;
    fn epoll_ctl(
        epfd: c_int,
        op: c_int,
        fd: c_int,
        event: *mut epoll_event,
    ) -> c_int;
}

/// Raw epoll control operation constants.
const EPOLL_CTL_ADD: c_int = 1;
const EPOLL_CTL_DEL: c_int = 2;

/// EPOLLIN | EPOLLET flag combination.
const EPOLL_IN_ET: u32 = 0x001 | (1 << 31);

/// Create a new epoll instance.  Returns the file descriptor on success.
fn epoll_create() -> Result<c_int, std::io::Error> {
    let fd = unsafe { epoll_create1(0) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(fd)
    }
}

/// Register a file descriptor with the epoll instance.
pub(super) fn epoll_add(
    epfd: c_int,
    fd: c_int,
    data: u64,
) -> Result<(), std::io::Error> {
    let mut event = epoll_event {
        events: EPOLL_IN_ET,
        u64: data,
    };
    let ret = unsafe { epoll_ctl(epfd, EPOLL_CTL_ADD, fd, &mut event) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Remove a file descriptor from the epoll instance.
pub(super) fn epoll_del(epfd: c_int, fd: c_int) -> Result<(), std::io::Error> {
    let ret =
        unsafe { epoll_ctl(epfd, EPOLL_CTL_DEL, fd, std::ptr::null_mut()) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Wait for events on the epoll instance.  Blocks indefinitely.
pub(super) fn epoll_wait_raw(
    epfd: c_int,
    events: &mut [epoll_event],
) -> Result<c_int, std::io::Error> {
    let ret = unsafe {
        libc_epoll_wait(
            epfd,
            events.as_mut_ptr(),
            events.len() as c_int,
            -1, // Block indefinitely.
        )
    };
    if ret >= 0 {
        Ok(ret)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Manages the lifecycle of an epoll file descriptor.
///
/// Wraps a raw fd in a type-safe wrapper that closes the fd on drop.
pub(super) struct EpollFd(c_int);

impl EpollFd {
    pub(super) fn new() -> Result<Self, std::io::Error> {
        epoll_create().map(EpollFd)
    }

    pub(super) fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Drop for EpollFd {
    fn drop(&mut self) {
        let _ = unsafe { libc::close(self.0) };
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Multi-device epoll integration test
    // -----------------------------------------------------------------------
    //
    // Verifies that epoll correctly multiplexes events from multiple file
    // descriptors.  We use pipe(2) fds as stand-ins for evdev devices, since
    // real devices require root uinput access in test environments.

    /// RAII wrapper that closes a raw fd on drop.
    struct FdGuard(c_int);

    impl FdGuard {
        fn new(fd: c_int) -> Self {
            FdGuard(fd)
        }
    }

    impl Drop for FdGuard {
        fn drop(&mut self) {
            unsafe { libc::close(self.0) };
        }
    }

    /// Create a pipe and return (read_fd, write_fd) wrapped in guards.
    fn make_pipe() -> (FdGuard, FdGuard) {
        let mut fds: [c_int; 2] = [0; 2];
        let ret = unsafe { libc::pipe2(fds.as_mut_ptr(), 0) };
        assert_eq!(ret, 0, "pipe2 failed");
        (FdGuard::new(fds[0]), FdGuard::new(fds[1]))
    }

    #[test]
    fn epoll_multiplexes_multiple_devices() {
        // Create two pipe pairs to simulate two independent devices.
        let (rd_a, wr_a) = make_pipe();
        let (rd_b, wr_b) = make_pipe();

        // Set up epoll and register both read ends.
        let epfd = epoll_create().expect("epoll_create");
        let fd_a = rd_a.0;
        let fd_b = rd_b.0;
        epoll_add(epfd, fd_a, fd_a as u64).expect("epoll_add A");
        epoll_add(epfd, fd_b, fd_b as u64).expect("epoll_add B");

        // Write a byte to pipe A only.
        let buf_a: u8 = 42;
        let ret =
            unsafe { libc::write(wr_a.0, &buf_a as *const _ as *const _, 1) };
        assert!(ret >= 0, "write to pipe A failed");

        // epoll_wait should return one event for pipe A.
        let mut events = vec![epoll_event { events: 0, u64: 0 }; 8];
        let n = epoll_wait_raw(epfd, &mut events).expect("epoll_wait");
        assert_eq!(n, 1, "expected exactly one epoll event");
        assert_eq!(
            events[0].u64 as RawFd, fd_a,
            "event should come from pipe A"
        );

        // Drain the byte from pipe A.
        let mut buf = [0u8; 1];
        let _ = unsafe { libc::read(fd_a, buf.as_mut_ptr() as *mut _, 1) };

        // Write to pipe B and verify it's the one that triggers.
        let buf_b: u8 = 99;
        let ret =
            unsafe { libc::write(wr_b.0, &buf_b as *const _ as *const _, 1) };
        assert!(ret >= 0, "write to pipe B failed");

        let mut events = vec![epoll_event { events: 0, u64: 0 }; 8];
        let n = epoll_wait_raw(epfd, &mut events).expect("epoll_wait");
        assert_eq!(n, 1, "expected exactly one epoll event");
        assert_eq!(
            events[0].u64 as RawFd, fd_b,
            "event should come from pipe B"
        );

        // Clean up: remove from epoll, then let guards close fds.
        let _ = epoll_del(epfd, fd_a);
        let _ = epoll_del(epfd, fd_b);
    }
}
