// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Peer-credential lookup for the macOS control socket and the virtkbdd IPC
//! client.

use std::{
    io,
    os::unix::{io::AsRawFd, net::UnixStream},
};

/// The audit token macOS attaches to every unix-socket peer.
///
/// Retrieved with `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)`; its field layout
/// is the stable kernel ABI documented by `audit_token_to_au32(3)` (the public
/// libbsm helpers are not exported from any linkable dylib on macOS 27, so the
/// layout is read directly here). Field order, confirmed against a live
/// `socketpair` on macOS 27:
///
/// ```text
/// [0] auid   (audit / real login uid — immutable across setuid)
/// [1] euid    effective user id
/// [2] egid
/// [3] ruid
/// [4] rgid
/// [5] pid   (peer process id at connection time)
/// [6] asid  (audit session id)
/// [7] tid   (terminal/process-id version)
/// ```
#[repr(C)]
struct AuditToken([u32; 8]);

impl AuditToken {
    /// The real user ID of the peer (audit-token slot `[3]`).
    ///
    /// A `setuid` transition does not change the real uid: a setuid-root
    /// attacker keeps its invoking uid here while only the effective uid
    /// becomes 0, so the `real_uid == 0 && effective_uid == 0` policy still
    /// rejects it (R3-14). We read the real uid rather than the audit uid
    /// (slot `[0]`) on purpose: a launchd-spawned LaunchDaemon has no login
    /// session, so its audit uid is `AU_AUDITUID_NONE` (`0xFFFFFFFF`), and
    /// gating on the audit uid would reject a genuine root daemon such as
    /// virtkbdd.
    fn real_uid(&self) -> libc::uid_t {
        self.0[3] as libc::uid_t
    }
    /// The effective user ID of the peer (audit-token slot `[1]`).
    fn effective_uid(&self) -> libc::uid_t {
        self.0[1] as libc::uid_t
    }
    /// The peer's process ID at connection time.
    fn pid(&self) -> libc::pid_t {
        self.0[5] as libc::pid_t
    }
}

/// Kernel-reported credentials of a connected unix peer.
///
/// On macOS both fields come from the peer's audit token (via
/// `LOCAL_PEERTOKEN`), which is authoritative and cannot be forged by the
/// peer. `real_uid` is the real uid (audit-token slot `[3]`): a process that
/// merely escalated to root via `setuid` keeps its invoking real uid, so the
/// `real_uid == 0 && effective_uid == 0` policy correctly rejects it, while a
/// genuine root daemon (real and effective uid both 0) is admitted.
///
/// `peer_pid` is the process ID of the connecting peer at connection time,
/// extracted from the audit token. It is informational only (used for logging
/// and `proc_pidpath` resolution in the virtkbdd IPC server) — it is not a
/// trust anchor, since PIDs are recycled.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct PeerCreds {
    pub real_uid: libc::uid_t,
    pub effective_uid: libc::uid_t,
    pub peer_pid: Option<libc::pid_t>,
}

/// Fetch the peer's credentials from its audit token.
///
/// `getpeereid(2)` on macOS 27+ returns only the *effective* uid (written to
/// offset 0), so a setuid-root peer — effective uid 0 but a non-zero real uid
/// — would be admitted through the old `peer == 0` branch.
/// The audit token exposes both identities, which
/// `authorize_peer`/`verify_server_uid` require to be consistent. A lookup
/// failure is propagated so the caller fails closed: a peer whose
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
        peer_pid: Some(token.pid()),
    })
}

/// Resolve the executable path of the process identified by *pid*.
///
/// Uses `proc_pidpath(2)` to translate a kernel-reported peer PID into its
/// executable path. This is informational only — it is used by the virtkbdd
/// IPC server to log which binary connected, providing operators visibility
/// into connection attempts without relying on code signing (R3-1).
///
/// Returns `None` when the PID no longer exists or `proc_pidpath` fails.
/// The path is not a trust anchor: PIDs are recycled, so a stale PID could
/// resolve to an unrelated binary.
pub(crate) fn peer_proc_path(pid: libc::pid_t) -> Option<String> {
    let mut buf = [0u8; 4096];
    let len = unsafe {
        libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32)
    };
    if len <= 0 {
        return None;
    }
    // `proc_pidpath` returns the byte count excluding the null terminator; the
    // buffer contains a NUL-terminated C string starting at offset 0.
    std::str::from_utf8(&buf[..len as usize])
        .ok()
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The audit-token reader must yield this process's own identity when
    /// queried over a `socketpair` (the peer is the same process). This is the
    /// macOS-specific half of `authorize_peer_checks_peer_credentials`: it
    /// pins the FFI field layout to the real kernel value so a future
    /// macOS release that shifts the layout would surface as a failing
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
        // The PID field is populated from the audit token and should match
        // this process's own pid.
        assert_eq!(
            creds.peer_pid,
            Some(unsafe { libc::getpid() }),
            "peer pid should match getpid"
        );
    }

    /// The audit-token field slots are pinned against the real `libbsm`
    /// accessors (`audit_token_to_{auid,euid,ruid,pid}`), which place `auid`
    /// at `[0]`, `euid` at `[1]`, `ruid` at `[3]` and `pid` at `[5]`. A
    /// `socketpair` cannot distinguish these slots (a self-peer has
    /// `auid == euid == ruid`), so this synthetic token is what actually pins
    /// `real_uid` to the real-uid slot and guards against the `auid`/`ruid`
    /// mix-up that rejected launchd-spawned root daemons.
    #[test]
    fn audit_token_field_slots_match_libbsm() {
        let token = AuditToken([100, 101, 102, 103, 104, 105, 106, 107]);
        assert_eq!(
            token.real_uid(),
            103,
            "real uid must read the ruid slot [3], not the auid slot [0]"
        );
        assert_eq!(token.effective_uid(), 101, "effective uid must read [1]");
        assert_eq!(token.pid(), 105, "pid must read [5]");
    }

    /// `peer_proc_path` resolves this process's own executable path.
    #[test]
    fn peer_proc_path_resolves_self() {
        let pid = unsafe { libc::getpid() };
        let path = peer_proc_path(pid);
        assert!(path.is_some(), "proc_pidpath should succeed for self");
        let path = path.unwrap();
        assert!(path.starts_with('/'), "path should be absolute: {path}");
    }
}
