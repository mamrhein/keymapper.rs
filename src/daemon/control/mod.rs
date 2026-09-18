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
//! Wire format. A frame reuses the versioned length-prefix idea from the
//! macOS emitter channel and carries a single UTF-8 command line with no
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

use std::io::{ErrorKind, Read, Write};

use log::LevelFilter;
use thiserror::Error;

use super::logging;

/// A stream that is both a [`Read`] and a [`Write`].
///
/// Rust forbids two non-auto traits in one object position, so a connected
/// endpoint is boxed as `Box<dyn IoStream>`; the blanket impl lets any
/// `Read + Write` type satisfy it.
pub(crate) trait IoStream: Read + Write {}
impl<T: Read + Write + ?Sized> IoStream for T {}

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::start;
#[cfg(unix)]
use unix::{connect, connect_error};
#[cfg(windows)]
pub use windows::start;
#[cfg(windows)]
use windows::{connect, connect_error};

/// The only supported frame version.
const FRAME_VERSION: u8 = 1;

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

    /// The frame version is not [`FRAME_VERSION`].
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
pub(crate) fn handle_connection<R: Read + Write>(
    io: &mut R,
) -> Result<(), ControlError> {
    let command = read_frame(io)?;
    let reply = dispatch(&command);
    write_frame(io, &reply.to_string())
}

/// Parse a command payload and run it, returning the reply to send back.
///
/// `SET-LOG-LEVEL` is implemented; the reserved verbs are recognised so a
/// future daemon stops reporting them as unknown but has no action yet;
/// anything else is an unknown command.
pub(crate) fn dispatch(command: &str) -> Response {
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

/// Encode *payload* as a single frame.
///
/// The payload is truncated to [`MAX_PAYLOAD`] before framing. Commands and
/// replies are far shorter than the bound, so this is defensive only.
pub(crate) fn encode_frame(payload: &str) -> Vec<u8> {
    let bytes = payload.as_bytes();
    let bytes = &bytes[..bytes.len().min(MAX_PAYLOAD)];

    let mut frame = Vec::with_capacity(5 + bytes.len());
    frame.push(FRAME_VERSION);
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    frame
}

/// Write one frame (encoding *payload*) to *writer*.
pub(crate) fn write_frame<W: Write>(
    writer: &mut W,
    payload: &str,
) -> Result<(), ControlError> {
    let frame = encode_frame(payload);
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

/// Read and decode one frame from *reader*, returning the payload as a
/// [`String`].
pub(crate) fn read_frame<R: Read>(
    reader: &mut R,
) -> Result<String, ControlError> {
    let mut header = [0u8; 5];
    read_exact_eof(reader, &mut header)?;

    let version = header[0];
    if version != FRAME_VERSION {
        return Err(ControlError::UnsupportedVersion(version));
    }

    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]])
        as usize;
    if len > MAX_PAYLOAD {
        return Err(ControlError::PayloadTooLarge(len));
    }

    let mut payload = vec![0u8; len];
    read_exact_eof(reader, &mut payload)?;
    String::from_utf8(payload).map_err(|_| ControlError::InvalidUtf8)
}

/// Read exactly `buf.len()` bytes, mapping a clean EOF to
/// [`ControlError::Eof`] so a peer close is distinguishable from an I/O
/// failure.
fn read_exact_eof<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
) -> Result<(), ControlError> {
    match reader.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
            Err(ControlError::Eof)
        }
        Err(e) => Err(ControlError::Io(e)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

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

    #[test]
    fn dispatch_sets_a_valid_level() {
        let response = dispatch("SET-LOG-LEVEL debug");
        assert_eq!(response.to_string(), "OK debug");
        // Restore the default so other tests observe it.
        logging::set_level(LevelFilter::Info);
    }

    #[test]
    fn dispatch_rejects_a_bad_level() {
        assert_eq!(
            dispatch("SET-LOG-LEVEL bogus").to_string(),
            "ERROR invalid level; expected error, warn, info, debug, or trace"
        );
    }

    #[test]
    fn dispatch_rejects_a_missing_level() {
        assert_eq!(
            dispatch("SET-LOG-LEVEL").to_string(),
            "ERROR invalid level; expected error, warn, info, debug, or trace"
        );
    }

    #[test]
    fn dispatch_rejects_an_empty_command() {
        assert_eq!(dispatch("   ").to_string(), "ERROR empty command");
    }

    #[test]
    fn dispatch_marks_reserved_verbs_unimplemented() {
        for verb in ["STATUS", "VERSION", "RELOAD"] {
            assert_eq!(
                dispatch(verb).to_string(),
                format!("ERROR {verb} is not implemented yet")
            );
        }
    }

    #[test]
    fn dispatch_rejects_unknown_verbs() {
        assert_eq!(dispatch("FLY").to_string(), "ERROR unknown command");
    }
}
