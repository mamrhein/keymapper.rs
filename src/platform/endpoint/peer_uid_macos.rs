// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Peer-credential lookup for the macOS control socket.

use std::{
    io,
    os::unix::{io::AsRawFd, net::UnixStream},
};

/// The macOS `struct xid` exchanged with `getpeereid(2)`.
///
/// macOS 27 changed the prototype from `(int, struct xid *)` to
/// `(int, uid_t *, gid_t *)`. Passing the first and third fields of this
/// struct as the two out-pointers is correct under both ABIs: the classic
/// form writes all three fields through the first pointer, while the new
/// form writes the euid and the gid to offsets 0 and 8. Only offset 0 is
/// read back, so the call stays within the struct on every release.
///
/// (Same layout trick as the virtkbdd IPC server's `peer_uid`.)
#[repr(C)]
struct Xid {
    xi_uid: libc::uid_t,
    xi_euid: libc::uid_t,
    xi_gid: libc::gid_t,
}

/// Return the uid of the connected peer, via `getpeereid(2)`.
///
/// Offset 0 of [`Xid`] holds the effective uid on macOS 27+ and the real
/// uid on earlier releases. For a normal user process such as keymapperd
/// the two are equal, and it is the identity that matters here.
pub(super) fn peer_uid(stream: &UnixStream) -> Result<libc::uid_t, String> {
    let mut xid = Xid {
        xi_uid: 0,
        xi_euid: 0,
        xi_gid: 0,
    };
    let status = unsafe {
        libc::getpeereid(stream.as_raw_fd(), &mut xid.xi_uid, &mut xid.xi_gid)
    };
    if status != 0 {
        return Err(format!("getpeereid: {}", io::Error::last_os_error()));
    }
    Ok(xid.xi_uid)
}
