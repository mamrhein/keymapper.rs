// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Peer-credential lookup for the Linux control socket.

use std::{
    io,
    os::unix::{io::AsRawFd, net::UnixStream},
};

/// Return the uid of the connected peer, or an error if the kernel cannot
/// report it (the caller then fails closed).
pub(super) fn peer_uid(stream: &UnixStream) -> Result<libc::uid_t, String> {
    // `SO_PEERCRED` is filled in by the kernel at connect time and cannot
    // be forged by the peer.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(cred).cast::<libc::c_void>(),
            &mut len,
        )
    };
    if status != 0 {
        return Err(format!(
            "getsockopt(SO_PEERCRED): {}",
            io::Error::last_os_error()
        ));
    }
    Ok(cred.uid)
}
