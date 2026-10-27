// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! wlroots-based compositors (sway, niri) — IPC socket query.
//!
//! Sway and other wlroots-based compositors expose a JSON IPC protocol
//! over a Unix domain socket.  We require `SWAYSOCK` to be set — falling
//! back to a hardcoded `/run/user/1000` would trust the wrong user's
//! socket when the daemon runs under a different uid.

use std::{
    io::{Read, Write},
    time::Duration,
};

use super::extract_json_field;

/// Query the active window on a wlroots-based compositor via its IPC socket.
///
/// Requires `SWAYSOCK` to be set; returns an empty string otherwise.
pub(super) fn query_wlroots() -> String {
    let socket = match std::env::var("SWAYSOCK") {
        Ok(s) if !s.is_empty() => s,
        _ => return String::new(),
    };

    if !std::path::Path::new(&socket).exists() {
        return String::new();
    }

    // Build the IPC get_tree message: length-prefixed binary protocol.
    let payload = r#"{"payload":"", "cmd":"get_tree"}"#;

    let mut msg = Vec::with_capacity(12 + payload.len());
    msg.extend_from_slice(b"sway"); // magic
    msg.extend_from_slice(&4u32.to_le_bytes()); // version
    msg.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // length
    msg.extend_from_slice(payload.as_bytes()); // payload

    query_wlroots_socket(&socket, &msg)
}

fn query_wlroots_socket(socket: &str, request: &[u8]) -> String {
    let mut stream = match std::os::unix::net::UnixStream::connect(socket) {
        Ok(s) => {
            s.set_read_timeout(Some(Duration::from_millis(50))).ok();
            s
        }
        Err(_) => return String::new(),
    };

    if stream.write_all(request).is_err() {
        return String::new();
    }

    let mut response = Vec::new();
    if stream.read_to_end(&mut response).is_err() {
        return String::new();
    }

    parse_wlroots_response(&response, "app_id")
}

/// Parse a length-prefixed sway/wlroots IPC response and extract the first
/// string value for `key` from the JSON payload.
///
/// The IPC response uses the same length-prefixed binary protocol as the
/// request: 4-byte "sway" magic, 4-byte version, 4-byte payload length, then
/// the JSON payload.
///
/// `checked_add` on `12 + payload_len` prevents a 32-bit overflow when a
/// forged length from a malicious socket reply would wrap past the bounds
/// check (then a slice panic, not corruption — the sink is same-uid in normal
/// deploys, but R3-9 makes this reachable from a relative-path socket).
fn parse_wlroots_response(response: &[u8], key: &str) -> String {
    if response.len() < 12 {
        return String::new();
    }

    // Verify the magic bytes.
    if &response[0..4] != b"sway" {
        return String::new();
    }

    let payload_len = u32::from_le_bytes([
        response[8],
        response[9],
        response[10],
        response[11],
    ]) as usize;

    let json_end = match 12usize.checked_add(payload_len) {
        Some(end) if response.len() >= end => end,
        _ => return String::new(),
    };

    let json = &response[12..json_end];
    let Ok(json_str) = std::str::from_utf8(json) else {
        return String::new();
    };
    extract_json_field(json_str, key)
}

#[cfg(test)]
mod tests {
    use super::parse_wlroots_response;

    /// Build a minimal 12-byte sway envelope with the given payload length in
    /// the header, optionally followed by payload bytes.
    fn build_envelope(payload_len: u32, payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(12 + payload.len());
        buf.extend_from_slice(b"sway"); // magic
        buf.extend_from_slice(&0u32.to_le_bytes()); // version
        buf.extend_from_slice(&payload_len.to_le_bytes()); // length
        buf.extend_from_slice(payload);
        buf
    }

    #[test]
    fn parse_response_extracts_field() {
        // Valid envelope + JSON payload: field is extracted.
        let payload = r#"{"app_id": "terminal"}"#;
        let buf = build_envelope(payload.len() as u32, payload.as_bytes());
        assert_eq!(parse_wlroots_response(&buf, "app_id"), "terminal");
    }

    #[test]
    fn parse_response_too_short() {
        // Less than 12 bytes: header incomplete, return empty.
        assert_eq!(parse_wlroots_response(b"sway", "app_id"), "");
        assert_eq!(parse_wlroots_response(&[0u8; 11], "app_id"), "");
    }

    #[test]
    fn parse_response_bad_magic() {
        // Magic bytes must be "sway".
        let payload = r#"{"app_id": "terminal"}"#;
        let mut buf = build_envelope(payload.len() as u32, payload.as_bytes());
        buf[0..4].copy_from_slice(b"sawx");
        assert_eq!(parse_wlroots_response(&buf, "app_id"), "");
    }

    #[test]
    fn parse_response_length_exceeds_buffer() {
        // Header claims more bytes than the buffer actually contains.
        let buf = build_envelope(999, b"short");
        assert_eq!(parse_wlroots_response(&buf, "app_id"), "");
    }

    #[test]
    fn parse_response_overflow_safe_on_32bit() {
        // On 32-bit `usize`, `12 + payload_len` where `payload_len` is near
        // `usize::MAX` would wrap past the bounds check without
        // `checked_add`.  We simulate the 32-bit condition: when `usize`
        // is 32-bit wide, a payload_len of `u32::MAX` makes `12 + len`
        // overflow.
        let payload_len = if usize::MAX == u32::MAX as usize {
            // 32-bit: u32::MAX would overflow 12 + len.
            u32::MAX
        } else {
            // 64-bit: use a length that exceeds the buffer without
            // overflowing.
            u32::MAX
        };
        let buf = build_envelope(payload_len, b"x");
        // Regardless of overflow, the response is far too short.
        assert_eq!(parse_wlroots_response(&buf, "app_id"), "");
    }
}
