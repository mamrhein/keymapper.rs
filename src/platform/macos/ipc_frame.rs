// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Length-prefixed framing for the keymapperd ↔ virtkbdd IPC socket.
//!
//! A frame is a single batch of mapped-output keys to emit:
//!
//! ```text
//! [u8 version = 1][u32 LE payload_len][payload]
//! payload = [u32 LE key_count][key_count × (u8 modifiers, u32 LE hid_code)]
//! ```
//!
//! `hid_code` is the [`HidUsage`] discriminant `(page << 16) | id`, which
//! round-trips through [`HidUsage::from_code`].  A frame carries at most
//! [`MAX_KEYS_PER_FRAME`] keys (64 × 5 bytes + 4 = 328 payload bytes); the
//! [`MAX_PAYLOAD_LEN`] bound (4096) is a generous guard against corrupt length
//! fields.  The codec is hand-rolled and trivially testable, so no external
//! serialization dependency is introduced.

use std::io::{ErrorKind, Read};

use thiserror::Error;

use crate::{common::hid_usage::HidUsage, daemon::mapping_cache::NativeKey};

/// The only supported frame version.
const FRAME_VERSION: u8 = 1;

/// Upper bound for a single frame's payload.  Legitimate payloads are at most
/// 328 bytes; the bound guards against corrupt length fields.
const MAX_PAYLOAD_LEN: usize = 4096;

/// Maximum number of keys in a single frame.
const MAX_KEYS_PER_FRAME: usize = 64;

/// Errors that can occur while decoding an IPC frame.
#[derive(Debug, Error)]
pub enum IpcFrameError {
    /// The frame buffer is shorter than the 5-byte header.
    #[error("frame too short: {0} bytes")]
    TooShort(usize),

    /// The frame version is not [`FRAME_VERSION`].
    #[error("unsupported frame version {0}")]
    UnsupportedVersion(u8),

    /// The declared payload length exceeds [`MAX_PAYLOAD_LEN`].
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

    /// The frame declares more keys than [`MAX_KEYS_PER_FRAME`].
    #[error("too many keys in frame: {0}")]
    TooManyKeys(usize),

    /// A key's HID code does not resolve to a recognized [`HidUsage`].
    #[error("unknown HID usage code {0:#010x}")]
    UnknownUsage(u32),

    /// The stream ended cleanly (the peer closed the connection).
    #[error("connection closed")]
    Eof,

    /// An underlying I/O error while reading the stream.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

// `std::io::Error` is not comparable, so derive is off the table; compare the
// structured variants field by field and treat any two I/O errors as equal.
impl PartialEq for IpcFrameError {
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
            (Self::TooManyKeys(a), Self::TooManyKeys(b)) => a == b,
            (Self::UnknownUsage(a), Self::UnknownUsage(b)) => a == b,
            (Self::Eof, Self::Eof) => true,
            (Self::Io(_), Self::Io(_)) => true,
            _ => false,
        }
    }
}

/// Encode a batch of keys as a single frame.
///
/// Batches longer than [`MAX_KEYS_PER_FRAME`] are truncated to the bound; the
/// tap callback never produces such a batch, so this is defensive only.
pub fn encode(keys: &[NativeKey]) -> Vec<u8> {
    let key_count = keys.len().min(MAX_KEYS_PER_FRAME);
    let payload_len = 4 + key_count * 5;

    let mut frame = Vec::with_capacity(5 + payload_len);
    frame.push(FRAME_VERSION);
    frame.extend_from_slice(&(payload_len as u32).to_le_bytes());
    frame.extend_from_slice(&(key_count as u32).to_le_bytes());
    for key in keys.iter().take(key_count) {
        frame.push(key.modifiers);
        frame.extend_from_slice(&key.usage.code().to_le_bytes());
    }
    frame
}

/// Decode a complete frame buffer produced by [`encode`].
pub fn decode(frame: &[u8]) -> Result<Vec<NativeKey>, IpcFrameError> {
    let (version, rest) =
        frame.split_first().ok_or(IpcFrameError::TooShort(0))?;
    if *version != FRAME_VERSION {
        return Err(IpcFrameError::UnsupportedVersion(*version));
    }
    if rest.len() < 4 {
        return Err(IpcFrameError::TooShort(frame.len()));
    }

    let payload_len =
        u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(IpcFrameError::PayloadTooLarge(payload_len));
    }
    let payload =
        rest.get(4..4 + payload_len)
            .ok_or(IpcFrameError::Truncated {
                need: 4 + payload_len,
                have: rest.len(),
            })?;

    decode_payload(payload)
}

/// Read and decode a single frame from a stream.
///
/// Returns [`IpcFrameError::Eof`] when the peer closes the connection cleanly.
pub fn decode_stream<R: Read>(
    reader: &mut R,
) -> Result<Vec<NativeKey>, IpcFrameError> {
    let mut header = [0u8; 5];
    read_exact_eof(reader, &mut header)?;

    let payload_len =
        u32::from_le_bytes([header[1], header[2], header[3], header[4]])
            as usize;
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(IpcFrameError::PayloadTooLarge(payload_len));
    }

    let mut payload = vec![0u8; payload_len];
    read_exact_eof(reader, &mut payload)?;

    let mut frame = Vec::with_capacity(5 + payload_len);
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&payload);

    decode(&frame)
}

/// Read exactly `buf.len()` bytes, mapping a clean EOF to
/// [`IpcFrameError::Eof`] so the server can distinguish a peer close from a
/// real I/O failure.
fn read_exact_eof<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
) -> Result<(), IpcFrameError> {
    match reader.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
            Err(IpcFrameError::Eof)
        }
        Err(e) => Err(IpcFrameError::Io(e)),
    }
}

/// Decode the payload portion of a frame (after the 5-byte header).
fn decode_payload(payload: &[u8]) -> Result<Vec<NativeKey>, IpcFrameError> {
    if payload.len() < 4 {
        return Err(IpcFrameError::Truncated {
            need: 4,
            have: payload.len(),
        });
    }

    let key_count =
        u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]])
            as usize;
    if key_count > MAX_KEYS_PER_FRAME {
        return Err(IpcFrameError::TooManyKeys(key_count));
    }

    let expected = 4 + key_count * 5;
    if payload.len() < expected {
        return Err(IpcFrameError::Truncated {
            need: expected,
            have: payload.len(),
        });
    }

    let mut keys = Vec::with_capacity(key_count);
    for i in 0..key_count {
        let off = 4 + i * 5;
        let modifiers = payload[off];
        let hid_code = u32::from_le_bytes([
            payload[off + 1],
            payload[off + 2],
            payload[off + 3],
            payload[off + 4],
        ]);
        let usage = HidUsage::from_code(hid_code)
            .ok_or(IpcFrameError::UnknownUsage(hid_code))?;
        keys.push(NativeKey { modifiers, usage });
    }
    Ok(keys)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn key(modifiers: u8, usage: HidUsage) -> NativeKey {
        NativeKey { modifiers, usage }
    }

    #[test]
    fn round_trip_single_key() {
        let keys = vec![key(0, HidUsage::A)];
        let frame = encode(&keys);
        assert_eq!(decode(&frame).unwrap(), keys);
    }

    #[test]
    fn round_trip_modifier_and_key() {
        // Left Shift (bit 1) + 'E' (usage 0x0E), the canonical mapped output.
        let keys = vec![key(0x02, HidUsage::E)];
        let frame = encode(&keys);
        assert_eq!(decode(&frame).unwrap(), keys);
    }

    #[test]
    fn round_trip_consumer_page() {
        // A consumer-page usage must survive the round trip with its page
        // intact (the discriminant encodes the page in the high 16 bits).
        let usage = HidUsage::consumer(0xCD).expect("valid consumer usage");
        let keys = vec![key(0, usage)];
        let frame = encode(&keys);
        assert_eq!(decode(&frame).unwrap(), keys);
    }

    #[test]
    fn round_trip_many_keys() {
        // The maximum batch size round-trips without loss.  Use the first 64
        // usages from the canonical table so every code is valid.
        let usages: Vec<HidUsage> = HidUsage::ALL
            .iter()
            .copied()
            .take(MAX_KEYS_PER_FRAME)
            .collect();
        assert_eq!(usages.len(), MAX_KEYS_PER_FRAME);
        let keys: Vec<NativeKey> = usages
            .iter()
            .enumerate()
            .map(|(i, &usage)| key((i % 8) as u8, usage))
            .collect();
        let frame = encode(&keys);
        assert_eq!(decode(&frame).unwrap(), keys);
    }

    #[test]
    fn empty_batch() {
        let frame = encode(&[]);
        // Header (5) + a 4-byte key-count field with count 0.
        assert_eq!(frame.len(), 9);
        assert_eq!(decode(&frame).unwrap(), Vec::<NativeKey>::new());
    }

    #[test]
    fn frame_layout_is_exact() {
        // Pin the wire layout: version, payload_len (LE), key_count (LE), then
        // one (modifiers, hid_code LE) pair per key.
        let frame = encode(&[key(0x02, HidUsage::E)]);
        let hid_code = HidUsage::E.code();
        assert_eq!(
            frame,
            [
                FRAME_VERSION,
                9,
                0,
                0,
                0, // payload_len = 4 + 1 * 5
                1,
                0,
                0,
                0,    // key_count = 1
                0x02, // modifiers: left shift
                hid_code as u8,
                (hid_code >> 8) as u8,
                (hid_code >> 16) as u8,
                (hid_code >> 24) as u8,
            ]
        );
    }

    #[test]
    fn decode_rejects_version_mismatch() {
        let mut frame = encode(&[key(0, HidUsage::A)]);
        frame[0] = FRAME_VERSION + 1;
        assert_eq!(
            decode(&frame).unwrap_err(),
            IpcFrameError::UnsupportedVersion(FRAME_VERSION + 1)
        );
    }

    #[test]
    fn decode_rejects_oversized_payload() {
        // Declare a payload length beyond the bound; the buffer is short, but
        // the length check must fire first.
        let mut frame = vec![FRAME_VERSION];
        frame.extend_from_slice(&((MAX_PAYLOAD_LEN + 1) as u32).to_le_bytes());
        assert_eq!(
            decode(&frame).unwrap_err(),
            IpcFrameError::PayloadTooLarge(MAX_PAYLOAD_LEN + 1)
        );
    }

    #[test]
    fn decode_rejects_truncated_payload() {
        let frame = encode(&[key(0, HidUsage::A), key(0, HidUsage::B)]);
        // Chop the second key off the frame.
        let truncated = &frame[..frame.len() - 5];
        assert!(matches!(
            decode(truncated).unwrap_err(),
            IpcFrameError::Truncated { .. }
        ));
    }

    #[test]
    fn decode_rejects_too_many_keys() {
        // Forge a payload whose key-count field declares more keys than the
        // bound allows.
        let payload = ((MAX_KEYS_PER_FRAME + 1) as u32).to_le_bytes().to_vec();
        let mut frame = vec![FRAME_VERSION];
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&payload);
        assert_eq!(
            decode(&frame).unwrap_err(),
            IpcFrameError::TooManyKeys(MAX_KEYS_PER_FRAME + 1)
        );
    }

    #[test]
    fn decode_rejects_unknown_usage() {
        // A well-formed frame whose HID code resolves to no known usage.
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // key_count = 1
        payload.push(0); // modifiers
        payload.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // bogus code
        let mut frame = vec![FRAME_VERSION];
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&payload);
        assert_eq!(
            decode(&frame).unwrap_err(),
            IpcFrameError::UnknownUsage(0xDEAD_BEEF)
        );
    }

    #[test]
    fn decode_rejects_short_header() {
        assert_eq!(decode(&[]).unwrap_err(), IpcFrameError::TooShort(0));
        assert_eq!(
            decode(&[FRAME_VERSION]).unwrap_err(),
            IpcFrameError::TooShort(1)
        );
    }

    #[test]
    fn decode_stream_round_trip() {
        use std::io::Cursor;

        let keys = vec![key(0x02, HidUsage::E), key(0, HidUsage::A)];
        let frame = encode(&keys);
        let mut cursor = Cursor::new(frame);
        assert_eq!(decode_stream(&mut cursor).unwrap(), keys);
    }

    #[test]
    fn decode_stream_eof_on_empty() {
        use std::io::Cursor;

        let mut cursor = Cursor::new(Vec::<u8>::new());
        assert_eq!(
            decode_stream(&mut cursor).unwrap_err(),
            IpcFrameError::Eof
        );
    }

    #[test]
    fn decode_stream_eof_on_truncated_header() {
        use std::io::Cursor;

        // A partial header (fewer than 5 bytes) is a clean EOF, not an I/O
        // error, because the peer closed mid-frame.
        let mut cursor = Cursor::new(vec![FRAME_VERSION, 9]);
        assert_eq!(
            decode_stream(&mut cursor).unwrap_err(),
            IpcFrameError::Eof
        );
    }
}
