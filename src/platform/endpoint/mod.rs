// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The OS-specific transport behind the daemon's runtime control endpoint.
//!
//! The daemon exposes a local endpoint the CLI uses to change the running
//! daemon's configuration at runtime (today only the log level). The wire
//! protocol — the framed command/response envelope and the command dispatch —
//! is platform-agnostic and lives in [`crate::daemon::control`]. This module
//! owns only the *transport*: binding and accepting the endpoint, connecting
//! to it as a client, and authenticating the peer with kernel-provided
//! credentials. Splitting the two keeps every OS-specific byte of the control
//! path under `platform/` (one home for platform code) while the protocol
//! above it stays shared.
//!
//! The [`Endpoint`] trait is the single boundary between them. It is consumed
//! by `daemon::control`, never the other way around: the daemon hands a
//! [`ConnectionHandler`] closure to [`Endpoint::start`], and the transport
//! invokes it once per authorized connection with a stream positioned for
//! framed I/O. The transport therefore never names `daemon` — it drives a
//! callback, which keeps the dependency arrow `daemon -> platform`.
//!
//! Auth is the endpoint itself, so it is the transport's responsibility: the
//! unix socket is created owner-only (`0600`, under a tightened umask) and its
//! parent directory validated, and every accepted peer is checked against the
//! kernel-reported uid (`SO_PEERCRED` on Linux, `getpeereid` on macOS); the
//! Windows named pipe carries an owner-only DACL. Reaching the endpoint is
//! already proof of ownership.

use std::io::{Read, Write};

/// A stream that is both a [`Read`] and a [`Write`].
///
/// Rust forbids two non-auto traits in one object position, so a connected
/// endpoint is boxed as `Box<dyn IoStream>`; the blanket impl lets any
/// `Read + Write` type satisfy it.
pub(crate) trait IoStream: Read + Write {}
impl<T: Read + Write + ?Sized> IoStream for T {}

/// Handle one authorized control connection end to end.
///
/// The daemon supplies this to [`Endpoint::start`]; the transport calls it
/// with a stream for a single request/response exchange. *peer_uid* is the
/// kernel-reported uid of the connecting peer (Unix) or `0` on Windows where
/// no uid concept exists. It returns `true` when the exchange completed and
/// the reply is buffered, so the transport may wait for the peer to drain it
/// before tearing the connection down, and `false` when it failed and the
/// transport should tear down immediately.
pub(crate) type ConnectionHandler = fn(&mut dyn IoStream, u32) -> bool;

/// Sliding-window rate limiter for control-socket connections.
///
/// Tracks connection attempts per peer uid over a sliding window and
/// rejects excess connections to prevent a single uid from flooding the
/// single-threaded accept loop and denying service to legitimate CLI
/// invocations (Finding 6). On Windows there is no uid concept, so all
/// connections share the same bucket.
pub(crate) struct RateLimiter {
    window: std::time::Duration,
    threshold: usize,
    entries: Vec<(u32, std::time::Instant)>,
}

impl RateLimiter {
    /// Create a limiter that allows at most *threshold* connections per
    /// *window* per uid.
    pub(crate) fn new(window: std::time::Duration, threshold: usize) -> Self {
        Self {
            window,
            threshold,
            entries: Vec::with_capacity(64),
        }
    }

    /// Returns `true` if the connection is allowed, `false` if the
    /// per-uid rate limit has been exceeded.  Old entries outside the
    /// window are pruned on every call, so the `Vec` stays bounded.
    pub(crate) fn check_and_record(&mut self, uid: u32) -> bool {
        let now = std::time::Instant::now();
        let cutoff = now - self.window;
        self.entries.retain(|(_, ts)| *ts >= cutoff);
        let count = self.entries.iter().filter(|(u, _)| *u == uid).count();
        if count >= self.threshold {
            return false;
        }
        self.entries.push((uid, now));
        true
    }
}

/// A local control endpoint: the OS-specific transport the daemon's framed
/// control protocol runs over.
///
/// The endpoint name (a unix socket path, or a per-run Windows pipe name
/// discovered through a published file) is resolved entirely inside each
/// implementation, from state both the daemon and the CLI derive the same way
/// from the environment. Callers never pass or parse a name, so the identity
/// of the endpoint is opaque above the transport.
pub(crate) trait Endpoint {
    /// Bind the endpoint and spawn a background thread that runs *handler*
    /// once per authorized connection.
    ///
    /// Binding is best-effort: the control endpoint is a convenience, so a
    /// bind failure is logged and returns without spawning the thread,
    /// leaving the daemon otherwise unaffected.
    fn start(handler: ConnectionHandler);

    /// Connect to the running daemon's endpoint as a client.
    fn connect() -> std::io::Result<Box<dyn IoStream>>;

    /// Classify a failed client connect into a message the CLI can show the
    /// user (e.g. distinguishing "no daemon is running" from "try again").
    fn connect_error(e: &std::io::Error) -> String;
}

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// The platform's control endpoint.
#[cfg(unix)]
pub(crate) use unix::UnixEndpoint as ControlEndpoint;
/// The platform's control endpoint.
#[cfg(windows)]
pub(crate) use windows::WindowsEndpoint as ControlEndpoint;
