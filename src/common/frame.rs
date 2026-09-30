// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Shared length-prefixed frame codec.
//!
//! Every local IPC channel in keymapper uses the same framing envelope: a
//! single header followed by an opaque payload.
//!
//! ```text
//! [u8 version = 1][u32 LE payload_len][payload]
//! ```
//!
//! The header layout, the version check, the length bound, and the clean-EOF
//! handling are defined once here so the consumers cannot drift.  Each
//! consumer layers its own payload semantics on top: the daemon control
//! socket ([`crate::daemon::control`]) frames a UTF-8 command line, while the
//! macOS emitter channel ([`crate::platform::macos`]) frames an encoded
//! batch of mapped-output keys.  Only the envelope is shared; the payload
//! codecs stay independent.
//!
//! [`parse`] decodes a complete frame buffer and validates it against the
//! caller's [`FrameError::PayloadTooLarge`] bound, returning the payload
//! slice; [`read_payload`] reads and validates one frame from a stream.

use std::io::{ErrorKind, Read, Write};

use thiserror::Error;

/// The only supported frame version.
pub(crate) const FRAME_VERSION: u8 = 1;

/// Errors raised by the shared frame envelope.
///
/// Consumers map these onto their own error types (adding payload-specific
/// variants such as invalid UTF-8 or unknown key codes) so the envelope
/// errors stay the single source of truth for framing while each protocol
/// keeps its public error surface.
#[derive(Debug, Error)]
pub(crate) enum FrameError {
    /// The frame buffer is shorter than the 5-byte header.
    #[error("frame too short: {0} bytes")]
    TooShort(usize),

    /// The frame version is not [`FRAME_VERSION`].
    #[error("unsupported frame version {0}")]
    UnsupportedVersion(u8),

    /// The declared payload length exceeds the caller's bound.
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

    /// The peer closed the stream cleanly before a full frame arrived.
    #[error("connection closed")]
    Eof,

    /// An underlying I/O error while reading or writing the stream.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// `std::io::Error` is not comparable, so derive is off the table; compare the
// structured variants field by field and treat any two I/O errors as equal.
impl PartialEq for FrameError {
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
            (Self::Eof, Self::Eof) => true,
            (Self::Io(_), Self::Io(_)) => true,
            _ => false,
        }
    }
}

/// Encode *payload* as a single frame.
///
/// The payload is framed verbatim; callers that impose their own payload-size
/// bound must truncate before calling.
pub(crate) fn encode(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(FRAME_VERSION);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// Write one frame (encoding *payload*) to *writer*, flushing when done.
pub(crate) fn write_frame<W: Write>(
    writer: &mut W,
    payload: &[u8],
) -> Result<(), FrameError> {
    writer.write_all(&encode(payload))?;
    writer.flush()?;
    Ok(())
}

/// Decode a complete frame buffer, validating the header against
/// *max_payload*, and return the payload slice.
///
/// A buffer shorter than the header is [`FrameError::TooShort`]; a buffer
/// whose declared length runs past its end is [`FrameError::Truncated`].
///
/// Consumed by the macOS emitter's buffer-based `decode`; unused on other
/// targets, which decode straight from the stream via [`read_payload`].
#[allow(dead_code)]
pub(crate) fn parse(
    frame: &[u8],
    max_payload: usize,
) -> Result<&[u8], FrameError> {
    let (version, rest) =
        frame.split_first().ok_or(FrameError::TooShort(0))?;
    if *version != FRAME_VERSION {
        return Err(FrameError::UnsupportedVersion(*version));
    }
    if rest.len() < 4 {
        return Err(FrameError::TooShort(frame.len()));
    }

    let len =
        u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
    if len > max_payload {
        return Err(FrameError::PayloadTooLarge(len));
    }
    rest.get(4..4 + len).ok_or(FrameError::Truncated {
        need: 4 + len,
        have: rest.len(),
    })
}

/// Read and validate one frame from *reader*, returning the payload bytes.
///
/// The *max_payload* bound guards against a corrupt length field from a
/// hostile or buggy peer.  A clean stream close before a full frame (partial
/// header or partial payload) is reported as [`FrameError::Eof`], keeping a
/// peer close distinguishable from an I/O failure.
pub(crate) fn read_payload<R: Read>(
    reader: &mut R,
    max_payload: usize,
) -> Result<Vec<u8>, FrameError> {
    let mut header = [0u8; 5];
    read_exact_eof(reader, &mut header)?;

    let version = header[0];
    if version != FRAME_VERSION {
        return Err(FrameError::UnsupportedVersion(version));
    }

    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]])
        as usize;
    if len > max_payload {
        return Err(FrameError::PayloadTooLarge(len));
    }

    let mut payload = vec![0u8; len];
    read_exact_eof(reader, &mut payload)?;
    Ok(payload)
}

/// Read exactly `buf.len()` bytes, mapping a clean EOF to
/// [`FrameError::Eof`] so a peer close is distinguishable from an I/O
/// failure.
fn read_exact_eof<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
) -> Result<(), FrameError> {
    match reader.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => Err(FrameError::Eof),
        Err(e) => Err(FrameError::Io(e)),
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
    fn round_trips_through_encode_and_parse() {
        let payload = b"hello";
        let frame = encode(payload);
        assert_eq!(parse(&frame, 256).unwrap(), payload);
    }

    #[test]
    fn round_trips_through_read_payload() {
        let frame = encode(b"SET-LOG-LEVEL debug");
        let mut cursor = Cursor::new(frame);
        assert_eq!(
            read_payload(&mut cursor, 256).unwrap(),
            b"SET-LOG-LEVEL debug"
        );
    }

    #[test]
    fn layout_is_exact() {
        assert_eq!(encode(b"OK"), [FRAME_VERSION, 2, 0, 0, 0, b'O', b'K']);
    }

    #[test]
    fn parse_rejects_short_header() {
        assert_eq!(parse(&[], 256).unwrap_err(), FrameError::TooShort(0));
        assert_eq!(
            parse(&[FRAME_VERSION], 256).unwrap_err(),
            FrameError::TooShort(1)
        );
    }

    #[test]
    fn parse_rejects_version_mismatch() {
        let mut frame = encode(b"OK");
        frame[0] = FRAME_VERSION + 1;
        assert_eq!(
            parse(&frame, 256).unwrap_err(),
            FrameError::UnsupportedVersion(FRAME_VERSION + 1)
        );
    }

    #[test]
    fn parse_rejects_oversized_payload() {
        let mut frame = vec![FRAME_VERSION];
        frame.extend_from_slice(&257u32.to_le_bytes());
        assert_eq!(
            parse(&frame, 256).unwrap_err(),
            FrameError::PayloadTooLarge(257)
        );
    }

    #[test]
    fn parse_rejects_truncated_payload() {
        let frame = encode(b"hello");
        assert!(matches!(
            parse(&frame[..frame.len() - 1], 256).unwrap_err(),
            FrameError::Truncated { .. }
        ));
    }

    #[test]
    fn read_payload_maps_clean_eof() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        assert_eq!(
            read_payload(&mut cursor, 256).unwrap_err(),
            FrameError::Eof
        );
    }

    #[test]
    fn read_payload_eof_on_partial_header() {
        let mut cursor = Cursor::new(vec![FRAME_VERSION, 2]);
        assert_eq!(
            read_payload(&mut cursor, 256).unwrap_err(),
            FrameError::Eof
        );
    }
}
