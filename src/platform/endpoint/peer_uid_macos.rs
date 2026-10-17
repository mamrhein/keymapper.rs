// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Peer-credential lookup for the macOs control socket and the virtkbdd IPC
//! client.

use std::{
    io,
    os::unix::{io::AsRawFd, net::UnixStream},
};

/// The audit token macOs attaches to every unix-socket peer.
///
/// Retrieved with `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)`; its field layout
/// is the stable kernel ABI documented by `audit_token_to_au32(3)` (the public
/// libbsm helpers are not exported from any linkable dylib on macOs 27, so the
/// layout is read directly here). Field order, confirmed against a live
/// `socketpair` on macOs 27:
///
/// ```text
/// [0] auid   (audit / real login uid — immutable across setuid)
/// [1] euid    effective user id
/// [2] egid
/// [3] ruid
/// [4] rgid
/// [5] pid
/// [6] asid  (audit session id)
/// [7] tid   (terminal/process-id version)
/// ```
#[repr(C)]
struct AuditToken([u32; 8]);

impl AuditToken {
    /// The audit user ID — the immutable real identity of the peer's login
    /// session. It is fixed at login and cannot be cleared by a `setuid`
    /// transition, which is exactly what distinguishes a genuine root daemon
    /// from a setuid-root attacker (R3-14).
    fn real_uid(&self) -> libc::uid_t {
        self.0[0] as libc::uid_t
    }
    /// The effective user ID of the peer.
    fn effective_uid(&self) -> libc::uid_t {
        self.0[1] as libc::uid_t
    }
}

/// Kernel-reported credentials of a connected unix peer.
///
/// On macOs both fields come from the peer's audit token (via
/// `LOCAL_PEERTOKEN`), which is authoritative and cannot be forged by the
/// peer. `real_uid` is the audit user ID: for a normal process it equals the
/// real uid, and because it is frozen at login it stays non-zero for a process
/// that merely escalated to root via `setuid` — so the `real_uid == 0 &&
/// effective_uid == 0` policy correctly rejects such peers (R3-14).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct PeerCreds {
    pub real_uid: libc::uid_t,
    pub effective_uid: libc::uid_t,
}

/// Fetch the peer's credentials from its audit token.
///
/// `getpeereid(2)` on macOs 27+ returns only the *effective* uid (written to
/// offset 0), so a setuid-root peer — effective uid 0 but a non-zero
/// real/audit identity — would be admitted through the old `peer == 0` branch.
/// The audit token exposes both identities, which
/// `authorize_peer`/`verify_server_uid` require to be consistent (R3-14). A
/// lookup failure is propagated so the caller fails closed: a peer whose
/// credentials cannot be established is never admitted.
pub(crate) fn peer_creds(stream: &UnixStream) -> Result<PeerCreds, String> {
    let mut token = AuditToken([0; 8]);
    let mut len = std::mem::size_of::<AuditToken>() as libc::socklen_t;
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            std::ptr::addr_of_mut!(token).cast::<libc::c_void>(),
            &mut len,
        )
    };
    if status != 0 {
        return Err(format!(
            "getsockopt(LOCAL_PEERTOKEN): {}",
            io::Error::last_os_error()
        ));
    }
    if len as usize != std::mem::size_of::<AuditToken>() {
        return Err(format!(
            "getsockopt(LOCAL_PEERTOKEN): short reply ({} bytes, expected {})",
            len,
            std::mem::size_of::<AuditToken>()
        ));
    }
    Ok(PeerCreds {
        real_uid: token.real_uid(),
        effective_uid: token.effective_uid(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The audit-token reader must yield this process's own identity when
    /// queried over a `socketpair` (the peer is the same process). This is the
    /// macOs-specific half of `authorize_peer_checks_peer_credentials`: it
    /// pins the FFI field layout to the real kernel value so a future
    /// macOs release that shifts the layout would surface as a failing
    /// assertion here rather than a silent security regression.
    #[test]
    fn peer_creds_reports_self_over_socketpair() {
        let (a, _b) = UnixStream::pair().unwrap();
        let creds = peer_creds(&a)
            .expect("LOCAL_PEERTOKEN should succeed on a socketpair");
        let uid = unsafe { libc::getuid() };
        assert_eq!(
            creds.real_uid, uid,
            "real (audit) uid should match getuid"
        );
        assert_eq!(
            creds.effective_uid, uid,
            "effective uid should match getuid"
        );
    }
}
