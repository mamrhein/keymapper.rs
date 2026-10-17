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
//! slice; [`read_payload`] reads and validates one frame from a stream.  For a
//! stream of frames, [`FrameReader`] owns the partial bytes of the frame in
//! flight so a mid-frame read timeout does not discard them and desync the
//! stream; it is reset only on a complete frame or a fatal error.

use std::io::{ErrorKind, Read, Write};

use thiserror::Error;

/// The only supported frame version.
pub(crate) const FRAME_VERSION: u8 = 1;

/// Header length in bytes: one version byte followed by a little-endian
/// `u32` payload length.
const HEADER_LEN: usize = 5;

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
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.push(FRAME_VERSION);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// Write one frame (encoding *payload*) to *writer*, flushing when done.
///
/// `?Sized` so a boxed `dyn` stream (the control endpoint's connected
/// transport) can be framed directly, not just a concrete reader/writer.
pub(crate) fn write_frame<W: Write + ?Sized>(
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

/// Incremental, resumable frame reader.
///
/// A stream codec that owns the partial bytes of the frame currently in
/// flight, so a read timeout (e.g. `SO_RCVTIMEO` firing mid-header or
/// mid-payload) does not discard the bytes already consumed and desync the
/// stream.  State is retained across calls to
/// [`read_payload`](FrameReader::read_payload) until a complete frame is
/// produced or a fatal error resets the reader; on either the header buffer
/// and payload offset are cleared, so the next frame starts cleanly.  A
/// mid-frame `WouldBlock` surfaces as [`FrameError::Io`] and leaves
/// the accumulated bytes in place for the caller to retry on the same reader.
pub(crate) struct FrameReader<'a, R: Read + ?Sized> {
    reader: &'a mut R,
    max_payload: usize,
    /// Partial header bytes accumulated from a previous, interrupted read.
    header: [u8; HEADER_LEN],
    /// Number of valid bytes currently held in [`header`](Self::header).
    header_filled: usize,
    /// Buffer for the in-progress payload; allocated once the header's length
    /// field is known, and `None` while idle.
    payload: Option<Vec<u8>>,
    /// Number of valid bytes currently held in [`payload`](Self::payload).
    payload_filled: usize,
}

impl<'a, R: Read + ?Sized> FrameReader<'a, R> {
    /// Wrap *reader* with framing state and a *max_payload* bound.
    ///
    /// The bound guards against a corrupt length field from a hostile or
    /// buggy peer.  Holding the reader across frames is what makes reads
    /// resumable: a timeout that fires mid-frame leaves the partial bytes in
    /// place for the next call instead of dropping them.
    pub(crate) fn new(reader: &'a mut R, max_payload: usize) -> Self {
        Self {
            reader,
            max_payload,
            header: [0; HEADER_LEN],
            header_filled: 0,
            payload: None,
            payload_filled: 0,
        }
    }

    /// Read and validate one frame from the wrapped stream, resuming any
    /// partial header or payload left by a previous interrupted call.
    ///
    /// A clean stream close before a full frame (partial header or partial
    /// payload) is reported as [`FrameError::Eof`], keeping a peer close
    /// distinguishable from an I/O failure.  A mid-frame timeout is
    /// [`FrameError::Io`] with `WouldBlock`, and the partial bytes are
    /// retained so the next call on the same reader resumes instead of
    /// desyncing.
    pub(crate) fn read_payload(&mut self) -> Result<Vec<u8>, FrameError> {
        // Header phase: resume from `header_filled` if a previous read was
        // interrupted by a timeout after consuming 1..4 header bytes.
        if self.header_filled < HEADER_LEN {
            read_into(self.reader, &mut self.header, &mut self.header_filled)?;
        }

        let version = self.header[0];
        if version != FRAME_VERSION {
            self.reset();
            return Err(FrameError::UnsupportedVersion(version));
        }

        let len = u32::from_le_bytes([
            self.header[1],
            self.header[2],
            self.header[3],
            self.header[4],
        ]) as usize;
        if len > self.max_payload {
            self.reset();
            return Err(FrameError::PayloadTooLarge(len));
        }

        // Payload phase: allocate on first entry for this frame, then resume
        // an interrupted read into the same buffer.  The length is
        // trusted only after the bound above.
        if self.payload.is_none() {
            self.payload = Some(vec![0u8; len]);
        }
        read_into(
            self.reader,
            self.payload.as_deref_mut().unwrap(),
            &mut self.payload_filled,
        )?;

        let payload = self.payload.take().unwrap();
        // Full frame consumed: clear state so the next frame starts fresh.
        self.header_filled = 0;
        self.payload_filled = 0;
        Ok(payload)
    }

    /// Drop any partial progress, returning the reader to its idle state.
    ///
    /// Called on protocol violations so a corrupt frame cannot bleed into the
    /// next one through retained bytes.
    fn reset(&mut self) {
        self.header_filled = 0;
        self.payload = None;
        self.payload_filled = 0;
    }
}

/// Read and validate one frame from *reader*, returning the payload bytes.
///
/// This is a one-shot convenience over [`FrameReader`]: each call builds fresh
/// framing state, so it suits transports that carry a single frame per
/// connection (such as the control socket).  For a stream of frames — where a
/// read timeout must not desync framing — hold a [`FrameReader`] across calls.
///
/// The *max_payload* bound guards against a corrupt length field from a
/// hostile or buggy peer.  A clean stream close before a full frame (partial
/// header or partial payload) is reported as [`FrameError::Eof`], keeping a
/// peer close distinguishable from an I/O failure.
pub(crate) fn read_payload<R: Read + ?Sized>(
    reader: &mut R,
    max_payload: usize,
) -> Result<Vec<u8>, FrameError> {
    let mut reader_state = FrameReader::new(reader, max_payload);
    reader_state.read_payload()
}

/// Fill `buf` from `reader`, resuming at `*filled`.
///
/// A clean EOF (`Ok(0)` or `UnexpectedEof`) is mapped to [`FrameError::Eof`]
/// so a peer close is distinguishable from an I/O failure.  `Interrupted` is
/// retried transparently, matching `read_exact`'s contract; a timeout
/// (`WouldBlock`) and other errors surface as [`FrameError::Io`], leaving
/// `*filled` at whatever was accumulated before the failure so the caller can
/// retry and resume on the same reader.
fn read_into<R: Read + ?Sized>(
    reader: &mut R,
    buf: &mut [u8],
    filled: &mut usize,
) -> Result<(), FrameError> {
    while *filled < buf.len() {
        match reader.read(&mut buf[*filled..]) {
            Ok(0) => return Err(FrameError::Eof),
            Ok(n) => *filled += n,
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                return Err(FrameError::Eof);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io::{Cursor, ErrorKind},
    };

    use super::*;

    /// A `Read` stub driven by a scripted sequence of steps.
    ///
    /// Each step either hands back a chunk of bytes or returns an error, so a
    /// test can model a peer that delivers a frame in pieces or trips a read
    /// timeout mid-frame.  Once the steps are exhausted the reader reports a
    /// clean EOF (`Ok(0)`).
    enum Step {
        Bytes(Vec<u8>),
        Error(std::io::Error),
    }

    struct StubReader {
        steps: VecDeque<Step>,
    }

    impl Read for StubReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.steps.pop_front() {
                None => Ok(0),
                Some(Step::Bytes(data)) => {
                    let n = data.len().min(buf.len());
                    buf[..n].copy_from_slice(&data[..n]);
                    if n < data.len() {
                        let rest = data[n..].to_vec();
                        self.steps.push_front(Step::Bytes(rest));
                    }
                    Ok(n)
                }
                Some(Step::Error(e)) => Err(e),
            }
        }
    }

    /// A stream timeout modeled as a `WouldBlock` error, matching the kind
    /// returned by `SO_RCVTIMEO` on a blocking socket read.
    fn timeout() -> std::io::Error {
        std::io::Error::new(ErrorKind::WouldBlock, "timed out")
    }

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

    #[test]
    fn free_read_payload_matches_reader() {
        // The one-shot free function must agree with the resumable reader.
        let frame = encode(b"GET");
        let mut cursor_a = Cursor::new(frame.clone());
        let mut cursor_b = Cursor::new(frame);
        assert_eq!(
            read_payload(&mut cursor_a, 256).unwrap(),
            FrameReader::new(&mut cursor_b, 256).read_payload().unwrap(),
        );
    }

    // -----------------------------------------------------------------------
    // Resumable `FrameReader` behavior (Finding R3-12).
    // -----------------------------------------------------------------------

    #[test]
    fn reader_resumes_a_header_split_across_reads() {
        // The frame arrives one byte at a time; the reader must accumulate the
        // 5-byte header across several reads before producing the payload.
        let frame = encode(b"GET");
        let mut steps = VecDeque::new();
        for byte in frame {
            steps.push_back(Step::Bytes(vec![byte]));
        }
        let mut reader = StubReader { steps };
        let mut fr = FrameReader::new(&mut reader, 256);
        assert_eq!(fr.read_payload().unwrap(), b"GET");
    }

    #[test]
    fn reader_resumes_after_a_mid_header_timeout() {
        // Two header bytes arrive, then a read timeout fires; the partial
        // header must be retained so the next call completes the frame.
        let frame = encode(b"GET");
        let mut steps = VecDeque::new();
        steps.push_back(Step::Bytes(vec![frame[0], frame[1]]));
        steps.push_back(Step::Error(timeout()));
        steps.push_back(Step::Bytes(vec![frame[2], frame[3], frame[4]]));
        steps.push_back(Step::Bytes(frame[5..].to_vec()));
        let mut reader = StubReader { steps };

        let mut fr = FrameReader::new(&mut reader, 256);
        let err = fr.read_payload().unwrap_err();
        assert!(matches!(
            err,
            FrameError::Io(ref e) if e.kind() == ErrorKind::WouldBlock
        ));
        // The retained header bytes are reused; the frame completes cleanly.
        assert_eq!(fr.read_payload().unwrap(), b"GET");
    }

    #[test]
    fn reader_resumes_after_a_mid_payload_timeout() {
        // The full header plus most of the payload arrive, then a timeout; the
        // partial payload must be retained so the next call finishes it.
        let frame = encode(b"GET");
        let mut steps = VecDeque::new();
        // version + length (5) + first two payload bytes ('G', 'E').
        steps.push_back(Step::Bytes(vec![
            frame[0], frame[1], frame[2], frame[3], frame[4], b'G', b'E',
        ]));
        steps.push_back(Step::Error(timeout()));
        // The remaining payload byte ('T') and nothing else.
        steps.push_back(Step::Bytes(vec![b'T']));
        let mut reader = StubReader { steps };

        let mut fr = FrameReader::new(&mut reader, 256);
        let err = fr.read_payload().unwrap_err();
        assert!(matches!(
            err,
            FrameError::Io(ref e) if e.kind() == ErrorKind::WouldBlock
        ));
        assert_eq!(fr.read_payload().unwrap(), b"GET");
    }

    #[test]
    fn reader_resumes_two_frames_across_a_timeout() {
        // Two back-to-back frames; a timeout lands inside the second header.
        // The reader must finish frame one, then complete frame two from the
        // retained second-header bytes.
        let frame_one = encode(b"AA");
        let frame_two = encode(b"BB");
        let mut steps = VecDeque::new();
        steps.push_back(Step::Bytes(frame_one));
        // Second header arrives one byte at a time with a timeout in the
        // middle.
        steps.push_back(Step::Bytes(vec![frame_two[0]]));
        steps.push_back(Step::Error(timeout()));
        steps.push_back(Step::Bytes(vec![frame_two[1], frame_two[2]]));
        steps.push_back(Step::Bytes(vec![frame_two[3], frame_two[4]]));
        steps.push_back(Step::Bytes(frame_two[5..].to_vec()));
        let mut reader = StubReader { steps };

        let mut fr = FrameReader::new(&mut reader, 256);
        assert_eq!(fr.read_payload().unwrap(), b"AA");
        let err = fr.read_payload().unwrap_err();
        assert!(matches!(
            err,
            FrameError::Io(ref e) if e.kind() == ErrorKind::WouldBlock
        ));
        assert_eq!(fr.read_payload().unwrap(), b"BB");
    }

    #[test]
    fn reader_clean_eof_after_a_partial_header_drops_state() {
        // A peer close mid-header is reported once as Eof; the free function
        // path exercises the same mapping through the shared reader.
        let mut cursor = Cursor::new(vec![FRAME_VERSION, 0, 0]);
        let mut fr = FrameReader::new(&mut cursor, 256);
        assert_eq!(fr.read_payload().unwrap_err(), FrameError::Eof);
    }

    #[test]
    fn reader_rejects_oversized_payload_and_resets() {
        // A length field exceeding the bound must reset framing state so a
        // subsequent read does not try to interpret payload bytes as a header.
        let mut steps = VecDeque::new();
        let mut bad = vec![FRAME_VERSION];
        bad.extend_from_slice(&((256u32).to_le_bytes()));
        steps.push_back(Step::Bytes(bad));
        // Then a clean, in-bounds frame.
        steps.push_back(Step::Bytes(encode(b"ok")));
        let mut reader = StubReader { steps };

        let mut fr = FrameReader::new(&mut reader, 4);
        assert_eq!(
            fr.read_payload().unwrap_err(),
            FrameError::PayloadTooLarge(256)
        );
        // State was reset: the next frame decodes cleanly.
        assert_eq!(fr.read_payload().unwrap(), b"ok");
    }

    #[test]
    fn reader_rejects_bad_version_and_resets() {
        let mut steps = VecDeque::new();
        steps.push_back(Step::Bytes(vec![FRAME_VERSION + 1, 0, 0, 0, 0]));
        steps.push_back(Step::Bytes(encode(b"ok")));
        let mut reader = StubReader { steps };

        let mut fr = FrameReader::new(&mut reader, 256);
        assert_eq!(
            fr.read_payload().unwrap_err(),
            FrameError::UnsupportedVersion(FRAME_VERSION + 1)
        );
        assert_eq!(fr.read_payload().unwrap(), b"ok");
    }
}
