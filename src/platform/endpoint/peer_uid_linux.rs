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

/// Kernel-reported credentials of a connected unix peer.
///
/// On Linux `SO_PEERCRED` exposes a single uid (the effective uid); the real
/// and effective identities are therefore equal, so both `PeerCreds` fields
/// are set to `cred.uid`. This keeps the macOs-style
/// `real_uid == 0 && effective_uid == 0` root policy identical to the previous
/// `peer == 0` check on Linux (no behavior change), while unifying the type
/// consumed by `authorize_peer`/`verify_server_uid` across the two unix OSes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct PeerCreds {
    pub real_uid: libc::uid_t,
    pub effective_uid: libc::uid_t,
}

/// Fetch the peer's credentials via `SO_PEERCRED`.
///
/// `SO_PEERCRED` is filled in by the kernel at connect time and cannot be
/// forged by the peer. The returned `len` is validated against
/// `size_of::<ucred>()`; a short-but-successful reply would leave the
/// zero-initialized `cred.uid` reading as uid 0, which the policy admits — a
/// fail-open toward the privileged uid. A short reply is therefore rejected so
/// that cannot happen (R3-10).
pub(crate) fn peer_creds(stream: &UnixStream) -> Result<PeerCreds, String> {
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
    // On a short-but-successful write the zero-initialized `cred.uid` reads as
    // uid 0, which the policy *admits* — a fail-open toward the privileged
    // uid. Validate the length so that cannot happen (R3-10).
    if len as usize != std::mem::size_of::<libc::ucred>() {
        return Err(format!(
            "getsockopt(SO_PEERCRED): short reply ({} bytes, expected {})",
            len,
            std::mem::size_of::<libc::ucred>()
        ));
    }
    Ok(PeerCreds {
        real_uid: cred.uid,
        effective_uid: cred.uid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `peer_creds` over a `socketpair` reports this process's own uid as both
    /// the real and effective identity (they are identical on Linux).
    #[test]
    fn peer_creds_reports_self_over_socketpair() {
        let (a, _b) = UnixStream::pair().unwrap();
        let creds = peer_creds(&a)
            .expect("SO_PEERCRED should succeed on a socketpair");
        let uid = unsafe { libc::getuid() };
        assert_eq!(creds.real_uid, uid);
        assert_eq!(creds.effective_uid, uid);
    }
}
