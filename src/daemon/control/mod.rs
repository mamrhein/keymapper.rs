// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Control socket: a local endpoint a running daemon opens so the CLI can
//! change its configuration at runtime without a restart.
//!
//! Today the only command is `SET-LOG-LEVEL`; the message space is left open
//! so `STATUS` / `VERSION` / `RELOAD` can be added later without a second
//! endpoint. The daemon binds the endpoint in [`start`] and serves one framed
//! request per connection from a small background thread. Reading logs stays
//! standard (journal / Event Viewer / the stderr fallback) — the socket only
//! changes the _level_, it never redirects output.
//!
//! **Auth is the endpoint itself.** The daemon binds it owner-only (unix
//! `0600`, a Windows pipe DACL scoped to the current user), so reaching the
//! socket is already proof of ownership. That supersedes the `daemon_token`
//! removed in Phase 2.
//!
//! Wire format. A frame uses the shared length-prefixed envelope from
//! `common::frame` and carries a single UTF-8 command line with no
//! terminating newline (the length prefix delimits it):
//!
//! ```text
//! [u8 version = 1][u32 LE payload_len][payload]
//! ```
//!
//! A request frame holds the command (e.g. `SET-LOG-LEVEL debug`); the
//! matching response frame holds the reply (`OK debug` or `ERROR <reason>`).
//! A connection carries exactly one request/response pair, then the peer
//! closes.
//!
//! Only the envelope is shared: the header codec, the version check, and the
//! payload-size bound live in `common::frame`, consumed here and by the macOS
//! emitter channel (macOS `ipc_frame`). This one still frames a
//! UTF-8 command line while the emitter frames an encoded batch of keys, so
//! the two payload formats — and the protocols above them — stay independent.
//!
//! This module is the platform-agnostic half of the control path: the framed
//! protocol and command dispatch. The OS-specific transport — the unix socket
//! and its `SO_PEERCRED`/`getpeereid` peer check, and the Windows named pipe
//! and its ACL machinery — lives in [`crate::platform::endpoint`] behind the
//! [`Endpoint`](crate::platform::endpoint::Endpoint) trait, which this module
//! consumes. [`start`] hands a [`serve_connection`] callback to the transport,
//! so the dependency arrow stays `daemon -> platform`: the transport runs the
//! protocol but never names it.

use std::io::{Read, Write};

use log::{LevelFilter, debug};
use thiserror::Error;

use super::logging;
use crate::{
    common::frame::{self, FrameError},
    platform::endpoint::{ControlEndpoint, Endpoint, IoStream},
};

/// Upper bound for a frame payload. A command or reply is at most a few dozen
/// bytes; the bound guards against a corrupt length field from a hostile or
/// buggy peer.
const MAX_PAYLOAD: usize = 256;

/// Command and framing errors on the control socket.
#[derive(Debug, Error)]
pub enum ControlError {
    /// The frame buffer is shorter than the 5-byte header.
    #[error("frame too short: {0} bytes")]
    TooShort(usize),

    /// The frame version is not the shared `common::frame` version.
    #[error("unsupported frame version {0}")]
    UnsupportedVersion(u8),

    /// The declared payload length exceeds [`MAX_PAYLOAD`].
    #[error("payload too large: {0} bytes")]
    PayloadTooLarge(usize),

    /// The frame is truncated: the declared length runs past the buffer end.
    #[error("truncated frame: need {need} bytes, have {have}")]
    Truncated {
        /// The number of bytes the frame declares it needs.
        need: usize,
        /// The number of bytes actually present.
        have: usize,
    },

    /// The payload bytes are not valid UTF-8.
    #[error("frame payload is not valid UTF-8")]
    InvalidUtf8,

    /// The peer closed the connection cleanly before a full frame arrived.
    #[error("connection closed")]
    Eof,

    /// No daemon control endpoint could be reached.
    #[error("could not connect to the control endpoint: {0}")]
    Connect(String),

    /// An underlying I/O error while reading or writing the stream.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// The frame envelope errors come from the shared `common::frame` codec; fold
// them into the matching control variants so the public error surface (and the
// tests that assert on it) stay unchanged while framing has one definition.
impl From<FrameError> for ControlError {
    fn from(e: FrameError) -> Self {
        match e {
            FrameError::TooShort(n) => Self::TooShort(n),
            FrameError::UnsupportedVersion(v) => Self::UnsupportedVersion(v),
            FrameError::PayloadTooLarge(n) => Self::PayloadTooLarge(n),
            FrameError::Truncated { need, have } => {
                Self::Truncated { need, have }
            }
            FrameError::Eof => Self::Eof,
            FrameError::Io(e) => Self::Io(e),
        }
    }
}

// `std::io::Error` is not comparable, so derive is off the table; compare the
// structured variants field by field and treat any two I/O errors as equal.
impl PartialEq for ControlError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::TooShort(a), Self::TooShort(b)) => a == b,
            (Self::UnsupportedVersion(a), Self::UnsupportedVersion(b)) => {
                a == b
            }
            (Self::PayloadTooLarge(a), Self::PayloadTooLarge(b)) => a == b,
            (
                Self::Truncated { need: an, have: ah },
                Self::Truncated { need: bn, have: bh },
            ) => an == bn && ah == bh,
            (Self::InvalidUtf8, Self::InvalidUtf8) => true,
            (Self::Eof, Self::Eof) => true,
            (Self::Connect(a), Self::Connect(b)) => a == b,
            (Self::Io(_), Self::Io(_)) => true,
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Server side
// ---------------------------------------------------------------------------

/// Bind the platform control endpoint and serve framed requests until the
/// process exits.
///
/// The bind and the accept loop are delegated to the platform transport
/// ([`ControlEndpoint`]); binding is best-effort, so a failure is logged there
/// and the daemon keeps running at the level it was seeded from. Each
/// authorized connection is served by [`serve_connection`], which speaks the
/// framed protocol defined in this module.
pub fn start() {
    ControlEndpoint::start(serve_connection);
}

/// Serve one authorized control connection.
///
/// *peer_uid* is the kernel-reported uid of the connecting peer (Unix) or
/// `0` on Windows where no uid concept exists. It is used to enforce uid-
/// dependent policy in [`dispatch`] (e.g. refusing root log-level
/// escalation on a user-run daemon).
///
/// Returns `true` on a completed exchange, so the transport may wait for the
/// peer to drain the reply before tearing the connection down (see
/// [`ConnectionHandler`](crate::platform::endpoint::ConnectionHandler)).
fn serve_connection(io: &mut dyn IoStream, peer_uid: u32) -> bool {
    match handle_connection(io, peer_uid) {
        Ok(()) => true,
        Err(e) => {
            // The peer is a well-behaved CLI; a failure is almost always the
            // peer closing early, so a debug note is enough.
            debug!("control-socket connection ended: {e}");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Client side
// ---------------------------------------------------------------------------

/// Connect to the running daemon's endpoint through the platform transport.
fn connect() -> std::io::Result<Box<dyn IoStream>> {
    ControlEndpoint::connect()
}

/// Classify a failed client connect into a message the CLI can show.
fn connect_error(e: &std::io::Error) -> String {
    ControlEndpoint::connect_error(e)
}

/// Connect to the running daemon's control endpoint and set its log level.
///
/// Returns the daemon's reply string (e.g. `OK debug`). A
/// [`ControlError::Connect`] means no daemon is reachable — either it is not
/// running or it predates the control socket; the CLI turns that into a
/// clear message.
pub fn set_log_level(level: LevelFilter) -> Result<String, ControlError> {
    let mut conn =
        connect().map_err(|e| ControlError::Connect(connect_error(&e)))?;
    exchange(&mut conn, &format!("SET-LOG-LEVEL {}", level_name(level)))
}

/// Send *command* over an already-connected stream and return the reply.
fn exchange<R: Read + Write>(
    io: &mut R,
    command: &str,
) -> Result<String, ControlError> {
    write_frame(io, command)?;
    read_frame(io)
}

/// Serve one connection: read a single request frame, dispatch it, and write
/// the reply frame. A connection carries exactly one command, so the peer
/// closes after the reply.
///
/// *peer_uid* is forwarded from [`serve_connection`] so [`dispatch`] can
/// enforce uid-dependent policy.
///
/// Generic over the stream and `?Sized` so it runs both on a concrete
/// transport stream and on the `dyn IoStream` the platform transport hands to
/// [`serve_connection`].
fn handle_connection<R: Read + Write + ?Sized>(
    io: &mut R,
    peer_uid: u32,
) -> Result<(), ControlError> {
    let command = read_frame(io)?;
    let reply = dispatch(&command, peer_uid);
    write_frame(io, &reply.to_string())
}

/// Parse a command payload and run it, returning the reply to send back.
///
/// *peer_uid* is the kernel-reported uid of the connecting peer (Unix) or
/// `0` on Windows. It gates the root log-level escalation refusal (Finding 4):
/// when a root peer asks for `debug` or `trace` on a daemon that is not
/// running as root, the request is refused to prevent keystroke logging
/// through the verbose emit path.
///
/// `SET-LOG-LEVEL` is implemented; the reserved verbs are recognised so a
/// future daemon stops reporting them as unknown but has no action yet;
/// anything else is an unknown command. Trailing arguments are never
/// silently ignored: a command with unexpected arguments is answered with
/// an error so CLI misuse stays visible.
pub(crate) fn dispatch(command: &str, peer_uid: u32) -> Response {
    let mut parts = command.split_whitespace();
    let Some(verb) = parts.next() else {
        return Response::Error("empty command".to_string());
    };
    match verb {
        "SET-LOG-LEVEL" => {
            let level =
                match parts.next().and_then(|a| a.parse::<LevelFilter>().ok())
                {
                    Some(level) => level,
                    None => {
                        return Response::Error(
                            "invalid level; expected error, warn, info, \
                             debug, or trace"
                                .to_string(),
                        );
                    }
                };
            // Reject trailing arguments rather than ignoring them:
            // `SET-LOG-LEVEL debug junk` answering `OK debug` would mask
            // CLI misuse.
            if let Some(extra) = parts.next() {
                return Response::Error(format!(
                    "unexpected argument '{extra}'"
                ));
            }
            // Security check: refuse `debug`/`trace` from a root peer when the
            // daemon is not itself running as root (Finding 4). At verbose
            // levels the daemon logs every key event with its resolved HID
            // usage, which is sufficient to reconstruct typed input.
            #[cfg(unix)]
            {
                if level >= LevelFilter::Debug
                    && peer_uid == 0
                    && unsafe { libc::getuid() } != 0
                {
                    return Response::Error(
                        "refusing to raise log level to debug/trace from
                         root on a non-root daemon"
                            .to_string(),
                    );
                }
            }
            logging::set_level(level);
            Response::Ok(level_name(level).to_string())
        }
        // Reserved command slots, kept out of the unknown path on purpose.
        "STATUS" | "VERSION" | "RELOAD" => {
            Response::Error(format!("{verb} is not implemented yet"))
        }
        _ => Response::Error("unknown command".to_string()),
    }
}

/// The reply to a request, rendered as a single-line response payload.
pub(crate) enum Response {
    /// The command succeeded; the value is echoed back.
    Ok(String),
    /// The command failed or is unimplemented; the value is the reason.
    Error(String),
}

impl std::fmt::Display for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Response::Ok(value) => write!(f, "OK {value}"),
            Response::Error(reason) => write!(f, "ERROR {reason}"),
        }
    }
}

/// Render a [`LevelFilter`] as its lowercase name, matching the command-line
/// spelling the CLI accepts.
fn level_name(level: LevelFilter) -> &'static str {
    match level {
        LevelFilter::Off => "off",
        LevelFilter::Error => "error",
        LevelFilter::Warn => "warn",
        LevelFilter::Info => "info",
        LevelFilter::Debug => "debug",
        LevelFilter::Trace => "trace",
    }
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Encode *payload* as a single frame (truncated to [`MAX_PAYLOAD`]).
///
/// Test-only helper: production writes go through [`write_frame`], which
/// truncates and frames inline. Kept here so the wire-layout and
/// truncation tests pin the exact bytes.
#[cfg(test)]
fn encode_frame(payload: &str) -> Vec<u8> {
    let bytes = payload.as_bytes();
    let bytes = &bytes[..bytes.len().min(MAX_PAYLOAD)];
    frame::encode(bytes)
}

/// Write one frame (encoding *payload*) to *writer*.
pub(crate) fn write_frame<W: Write + ?Sized>(
    writer: &mut W,
    payload: &str,
) -> Result<(), ControlError> {
    let bytes = payload.as_bytes();
    let bytes = &bytes[..bytes.len().min(MAX_PAYLOAD)];
    frame::write_frame(writer, bytes).map_err(ControlError::from)
}

/// Read and decode one frame from *reader*, returning the payload as a
/// [`String`].
///
/// The envelope is validated by the shared codec; this adds the UTF-8 check
/// specific to the command channel.
pub(crate) fn read_frame<R: Read + ?Sized>(
    reader: &mut R,
) -> Result<String, ControlError> {
    let payload = frame::read_payload(reader, MAX_PAYLOAD)
        .map_err(ControlError::from)?;
    String::from_utf8(payload).map_err(|_| ControlError::InvalidUtf8)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::common::frame::FRAME_VERSION;

    #[test]
    fn frame_round_trips_a_command() {
        let command = "SET-LOG-LEVEL debug";
        let frame = encode_frame(command);
        let mut cursor = Cursor::new(frame);
        assert_eq!(read_frame(&mut cursor).unwrap(), command);
    }

    #[test]
    fn frame_layout_is_exact() {
        // Pin the wire layout: version, payload_len (LE), payload.
        let frame = encode_frame("OK");
        assert_eq!(
            frame,
            [
                FRAME_VERSION,
                2,
                0,
                0,
                0, // payload_len = 2
                b'O',
                b'K',
            ]
        );
    }

    #[test]
    fn encode_truncates_oversized_payload() {
        let payload = "x".repeat(MAX_PAYLOAD + 100);
        let frame = encode_frame(&payload);
        let mut cursor = Cursor::new(frame);
        let decoded = read_frame(&mut cursor).unwrap();
        assert_eq!(decoded.len(), MAX_PAYLOAD);
    }

    #[test]
    fn read_frame_rejects_version_mismatch() {
        let mut frame = encode_frame("OK");
        frame[0] = FRAME_VERSION + 1;
        let mut cursor = Cursor::new(frame);
        assert_eq!(
            read_frame(&mut cursor).unwrap_err(),
            ControlError::UnsupportedVersion(FRAME_VERSION + 1)
        );
    }

    #[test]
    fn read_frame_rejects_oversized_payload() {
        // Declare a length beyond the bound; the length check must fire
        // before the (short) buffer is read.
        let mut frame = vec![FRAME_VERSION];
        frame.extend_from_slice(&((MAX_PAYLOAD + 1) as u32).to_le_bytes());
        let mut cursor = Cursor::new(frame);
        assert_eq!(
            read_frame(&mut cursor).unwrap_err(),
            ControlError::PayloadTooLarge(MAX_PAYLOAD + 1)
        );
    }

    #[test]
    fn read_frame_rejects_truncated_payload() {
        // Declare 10 bytes but provide only 4.
        let mut frame = vec![FRAME_VERSION];
        frame.extend_from_slice(&10u32.to_le_bytes());
        frame.extend_from_slice(b"abcd");
        let mut cursor = Cursor::new(frame);
        assert!(matches!(
            read_frame(&mut cursor).unwrap_err(),
            ControlError::Eof
        ));
    }

    #[test]
    fn read_frame_rejects_invalid_utf8() {
        let mut frame = vec![FRAME_VERSION];
        frame.extend_from_slice(&1u32.to_le_bytes());
        frame.push(0xFF); // not valid UTF-8
        let mut cursor = Cursor::new(frame);
        assert_eq!(
            read_frame(&mut cursor).unwrap_err(),
            ControlError::InvalidUtf8
        );
    }

    #[test]
    fn read_frame_eof_on_empty_stream() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        assert_eq!(read_frame(&mut cursor).unwrap_err(), ControlError::Eof);
    }

    #[test]
    fn read_frame_eof_on_partial_header() {
        let mut cursor = Cursor::new(vec![FRAME_VERSION, 2]);
        assert_eq!(read_frame(&mut cursor).unwrap_err(), ControlError::Eof);
    }

    // A non-root uid for tests that exercise the protocol without triggering
    // the root log-level escalation guard.
    const TEST_UID: u32 = 1;

    #[test]
    fn dispatch_sets_a_valid_level() {
        let response = dispatch("SET-LOG-LEVEL debug", TEST_UID);
        assert_eq!(response.to_string(), "OK debug");
        // Restore the default so other tests observe it.
        logging::set_level(LevelFilter::Info);
    }

    #[test]
    fn dispatch_rejects_a_bad_level() {
        assert_eq!(
            dispatch("SET-LOG-LEVEL bogus", TEST_UID).to_string(),
            "ERROR invalid level; expected error, warn, info, debug, or trace"
        );
    }

    #[test]
    fn dispatch_rejects_a_missing_level() {
        assert_eq!(
            dispatch("SET-LOG-LEVEL", TEST_UID).to_string(),
            "ERROR invalid level; expected error, warn, info, debug, or trace"
        );
    }

    #[test]
    fn dispatch_rejects_an_empty_command() {
        assert_eq!(
            dispatch("   ", TEST_UID).to_string(),
            "ERROR empty command"
        );
    }

    #[test]
    fn dispatch_marks_reserved_verbs_unimplemented() {
        for verb in ["STATUS", "VERSION", "RELOAD"] {
            assert_eq!(
                dispatch(verb, TEST_UID).to_string(),
                format!("ERROR {verb} is not implemented yet")
            );
        }
    }

    #[test]
    fn dispatch_rejects_unknown_verbs() {
        assert_eq!(
            dispatch("FLY", TEST_UID).to_string(),
            "ERROR unknown command"
        );
    }

    #[test]
    fn dispatch_rejects_trailing_arguments() {
        // The guard is about masking CLI misuse, so the reply must not
        // look like the command was accepted.
        assert_eq!(
            dispatch("SET-LOG-LEVEL debug junk", TEST_UID).to_string(),
            "ERROR unexpected argument 'junk'",
        );
        assert_eq!(
            dispatch("SET-LOG-LEVEL bogus junk", TEST_UID).to_string(),
            "ERROR invalid level; expected error, warn, info, debug, or trace",
        );
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_rejects_root_log_level_escalation() {
        // Skip when running as root: the guard only fires when the daemon
        // is NOT root, so as root the request is legitimately allowed.
        if unsafe { libc::getuid() } == 0 {
            return;
        }
        // A root peer asking for debug/trace on a non-root daemon must be
        // refused to prevent keystroke logging through the verbose emit
        // path (Finding 4).
        let reply = dispatch("SET-LOG-LEVEL debug", 0).to_string();
        assert!(
            reply.starts_with("ERROR"),
            "expected root debug escalation to be refused, got {reply}",
        );

        let reply = dispatch("SET-LOG-LEVEL trace", 0).to_string();
        assert!(
            reply.starts_with("ERROR"),
            "expected root trace escalation to be refused, got {reply}",
        );

        // A root peer asking for info must still be allowed.
        let reply = dispatch("SET-LOG-LEVEL info", 0).to_string();
        assert_eq!(reply, "OK info");
        logging::set_level(LevelFilter::Info);
    }
}
