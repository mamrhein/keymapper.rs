// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Client for the Karabiner DriverKit VirtualHIDDevice daemon.
//!
//! The Karabiner package ships a root daemon that owns the DriverKit virtual
//! HID extension.  Clients talk to it over a UNIX stream socket using a
//! length-prefixed frame protocol:
//!
//! ```text
//! [4-byte BE u32 body_size][1-byte msg_type][body]
//! ```
//!
//! where `body_size = 1 + len(body)`.  Request and response frames carry an
//! 8-byte big-endian request ID as the first body field; a request payload
//! is `[2-byte LE u16 protocol version][1-byte request type][report bytes]`.
//!
//! The daemon pushes state updates (driver activated, driver connected,
//! virtual keyboard ready, ...) as request frames; the client must answer
//! each one with an empty response frame or the daemon stalls its state
//! reporting.
//!
//! This client runs a dedicated background thread that owns the socket: it
//! connects to the daemon (retrying every second), initializes the virtual
//! keyboard, sends a heartbeat every 3 seconds, drops the connection if no
//! frame arrives within 30 seconds, and answers the daemon's state updates.
//! Reports are enqueued through [`KarabinerClient`] and written by the
//! background thread, so the public methods are safe to call from any
//! thread.

use std::{
    fmt,
    io::{ErrorKind, Read, Write},
    os::unix::net::UnixStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

// ---------------------------------------------------------------------------
// Protocol constants
// ---------------------------------------------------------------------------

/// Path of the Karabiner daemon's UNIX stream socket.
const SOCKET_PATH: &str = "/Library/Application \
                           Support/org.pqrs/tmp/rootonly/\
                           karabiner_virtual_hid_device_service.sock";

/// Client protocol version, embedded as a 2-byte little-endian u16 in every
/// request payload.
const PROTOCOL_VERSION: u16 = 7;

// Message types (the 1-byte field after the frame length).
const MSG_HEARTBEAT: u8 = 0;
const MSG_REQUEST: u8 = 4;
const MSG_RESPONSE: u8 = 5;

// Request types (the 1-byte field after the protocol version).
const REQ_KEYBOARD_INITIALIZE: u8 = 0;
const REQ_POST_KEYBOARD_INPUT_REPORT: u8 = 6;
const REQ_POST_CONSUMER_INPUT_REPORT: u8 = 7;

// State update types (the first byte of each [type][value] pair).
const RESP_DRIVER_ACTIVATED: u8 = 1;
const RESP_DRIVER_CONNECTED: u8 = 2;
const RESP_DRIVER_VERSION_MISMATCHED: u8 = 3;
const RESP_VIRTUAL_HID_KEYBOARD_READY: u8 = 4;

/// Interval between client heartbeats.
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(3000);

/// Drop the connection if no frame is received within this interval.
const READ_TIMEOUT: Duration = Duration::from_millis(30_000);

/// Per-operation write timeout.
const WRITE_TIMEOUT: Duration = Duration::from_millis(15_000);

/// Interval between reconnection attempts.
const RECONNECT_INTERVAL: Duration = Duration::from_millis(1000);

/// Socket poll interval.  The read timeout is kept short so that enqueued
/// reports are written to the socket promptly instead of waiting for the
/// next daemon frame or heartbeat.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Upper bound for a single frame.  Legitimate frames are at most a few
/// hundred bytes; the bound guards against corrupt length fields.
const MAX_FRAME_SIZE: usize = 64 * 1024;

/// Size of the packed `keyboard_input` report: report ID (1), modifiers
/// (1), reserved (1), and 32 × 2-byte little-endian usage slots.
const KEYBOARD_REPORT_SIZE: usize = 67;

/// Maximum number of simultaneous keys in a `keyboard_input` report.
const KEYBOARD_MAX_KEYS: usize = 32;

/// Size of the packed `consumer_input` report: report ID (1) and 32 ×
/// 2-byte little-endian usage slots (the same `keys` array as the keyboard
/// report).  The driver validates this exact size, so a shorter buffer is
/// rejected with a "buffer size error".
const CONSUMER_REPORT_SIZE: usize = 65;

/// Report ID of the `keyboard_input` report.
const KEYBOARD_REPORT_ID: u8 = 1;

/// Report ID of the `consumer_input` report.
const CONSUMER_REPORT_ID: u8 = 2;

/// The identity of a Karabiner DriverKit virtual keyboard as registered
/// with the daemon via `virtual_hid_keyboard_initialize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardIdentity {
    /// HID vendor ID.
    pub vendor_id: u64,
    /// HID product ID.
    pub product_id: u64,
    /// HID country code.
    pub country_code: u64,
}

/// The identity of the daemon's output keyboard.
///
/// This is the virtual keyboard the daemon creates to re-emit captured
/// keys.  It must be excluded from capture (see `iokit_hid`).
pub const OUTPUT_KEYBOARD_IDENTITY: KeyboardIdentity = KeyboardIdentity {
    vendor_id: 0x16c0,
    product_id: 0x27db,
    country_code: 0, // `not_supported`
};

/// The identity of the e2e injection keyboard.
///
/// The test harness creates a second virtual keyboard with this identity to
/// inject keystrokes; the daemon seizes it like any other physical keyboard.
/// The product ID is the next value after the output keyboard's `0x27db`;
/// the two-keyboard PoC verified that no other device in the keyboard set
/// shares it.
pub const INJECTION_KEYBOARD_IDENTITY: KeyboardIdentity = KeyboardIdentity {
    vendor_id: 0x16c0,
    product_id: 0x27dc,
    country_code: 0, // `not_supported`
};

/// Size of the `virtual_hid_keyboard_parameters` struct: three 8-byte
/// little-endian fields (vendor ID, product ID, country code).
const KEYBOARD_PARAMETERS_SIZE: usize = 24;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur while using the Karabiner client.
#[derive(Debug)]
pub enum KarabinerClientError {
    /// Failed to spawn the background client thread.
    ThreadSpawnFailed(std::io::Error),
    /// The background client thread has exited; commands can no longer be
    /// enqueued.
    ClientDisconnected,
}

impl fmt::Display for KarabinerClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ThreadSpawnFailed(e) => {
                write!(f, "failed to spawn the Karabiner client thread: {e}")
            }
            Self::ClientDisconnected => {
                write!(f, "the Karabiner client thread is no longer running")
            }
        }
    }
}

impl std::error::Error for KarabinerClientError {}

// ---------------------------------------------------------------------------
// Frame encoding / decoding
// ---------------------------------------------------------------------------

/// A decoded frame: the message type and the body (everything after the
/// message-type byte).
#[derive(Debug)]
struct Frame {
    msg_type: u8,
    body: Vec<u8>,
}

/// Encode a frame: `[4-byte BE u32 body_size][1-byte msg_type][body]`,
/// where `body_size = 1 + len(body)`.
fn encode_frame(msg_type: u8, body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + body.len());
    frame.extend_from_slice(&(1u32 + body.len() as u32).to_be_bytes());
    frame.push(msg_type);
    frame.extend_from_slice(body);
    frame
}

/// Encode a request or response frame whose body is an 8-byte big-endian
/// request ID followed by a payload.
fn encode_id_frame(msg_type: u8, request_id: u64, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + payload.len());
    body.extend_from_slice(&request_id.to_be_bytes());
    body.extend_from_slice(payload);
    encode_frame(msg_type, &body)
}

/// Decode a frame from `reader`.
fn decode_frame<R: Read>(reader: &mut R) -> std::io::Result<Frame> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let body_size = u32::from_be_bytes(len_buf) as usize;
    if !(1..=MAX_FRAME_SIZE).contains(&body_size) {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("invalid frame body size {body_size}"),
        ));
    }
    let mut body = vec![0u8; body_size];
    reader.read_exact(&mut body)?;
    Ok(Frame {
        msg_type: body[0],
        body: body[1..].to_vec(),
    })
}

/// Split a request/response body into the 8-byte big-endian request ID and
/// the remaining payload.
fn split_id_body(body: &[u8]) -> std::io::Result<(u64, &[u8])> {
    let (id_bytes, payload) = body.split_at_checked(8).ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            "request frame shorter than the 8-byte request ID",
        )
    })?;
    let request_id = u64::from_be_bytes(id_bytes.try_into().unwrap());
    Ok((request_id, payload))
}

/// Build a request payload: `[2-byte LE u16 protocol version][1-byte
/// request type][report bytes]`.
fn build_request_payload(request_type: u8, report: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(3 + report.len());
    payload.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    payload.push(request_type);
    payload.extend_from_slice(report);
    payload
}

/// Build the 24-byte `virtual_hid_keyboard_parameters` payload required by
/// the `virtual_hid_keyboard_initialize` request.
///
/// Layout: `[vendor_id: u64 LE][product_id: u64 LE][country_code: u64 LE]`.
/// The daemon rejects the request with a "buffer size error" unless this
/// payload is exactly `KEYBOARD_PARAMETERS_SIZE` bytes, so the virtual
/// keyboard never becomes ready without it.
fn build_keyboard_parameters(
    identity: &KeyboardIdentity,
) -> [u8; KEYBOARD_PARAMETERS_SIZE] {
    let mut params = [0u8; KEYBOARD_PARAMETERS_SIZE];
    params[0..8].copy_from_slice(&identity.vendor_id.to_le_bytes());
    params[8..16].copy_from_slice(&identity.product_id.to_le_bytes());
    params[16..24].copy_from_slice(&identity.country_code.to_le_bytes());
    params
}

// ---------------------------------------------------------------------------
// Report construction
// ---------------------------------------------------------------------------

/// Build a 67-byte `keyboard_input` report.
///
/// Layout: `[report_id=1][modifiers][reserved=0][32 × 2-byte LE u16
/// usages]`.  Each usage occupies the first free slot; usages beyond the
/// first 32 are dropped.
fn build_keyboard_input_report(
    modifiers: u8,
    usages: &[u16],
) -> [u8; KEYBOARD_REPORT_SIZE] {
    let mut report = [0u8; KEYBOARD_REPORT_SIZE];
    report[0] = KEYBOARD_REPORT_ID;
    report[1] = modifiers;
    for (slot, usage) in usages.iter().take(KEYBOARD_MAX_KEYS).enumerate() {
        report[3 + slot * 2..5 + slot * 2]
            .copy_from_slice(&usage.to_le_bytes());
    }
    report
}

/// Build a `consumer_input` report with the given Consumer Page usage in
/// the first slot of the keys array (the press field).
fn build_consumer_input_report(usage: u16) -> [u8; CONSUMER_REPORT_SIZE] {
    let mut report = [0u8; CONSUMER_REPORT_SIZE];
    report[0] = CONSUMER_REPORT_ID;
    report[1..3].copy_from_slice(&usage.to_le_bytes());
    report
}

/// Build an all-clear `consumer_input` report that releases any held
/// consumer key.
fn build_consumer_release_report() -> [u8; CONSUMER_REPORT_SIZE] {
    let mut report = [0u8; CONSUMER_REPORT_SIZE];
    report[0] = CONSUMER_REPORT_ID;
    report
}

// ---------------------------------------------------------------------------
// KarabinerClient
// ---------------------------------------------------------------------------

/// Commands enqueued for the background client thread.
enum ClientCommand {
    /// Post a `keyboard_input` report.
    KeyboardReport { modifiers: u8, usages: Vec<u16> },
    /// Post a `consumer_input` report with the usage in the press field.
    ConsumerPress { usage: u16 },
    /// Post an all-clear `consumer_input` report.
    ConsumerRelease,
}

/// A client connection to the Karabiner DriverKit VirtualHIDDevice daemon.
///
/// Created via [`KarabinerClient::connect`], which spawns a background
/// thread that owns the socket.  The thread connects to the daemon (retrying
/// every second), initializes the virtual keyboard, sends a heartbeat every
/// 3 seconds, drops the connection if no frame arrives within 30 seconds,
/// and answers the daemon's state-update requests.  Reports are enqueued
/// through [`KarabinerClient::send_keyboard_report`] and
/// [`KarabinerClient::send_consumer_report`].
pub struct KarabinerClient {
    tx: mpsc::Sender<ClientCommand>,
    ready: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
}

impl KarabinerClient {
    /// Probe whether the Karabiner service socket is reachable.
    ///
    /// Connects and immediately disconnects without sending any requests,
    /// so no virtual keyboard is created.  Fails when the socket does not
    /// exist or the caller lacks permission to connect (the socket is
    /// root-only).
    pub fn probe_socket() -> std::io::Result<()> {
        UnixStream::connect(SOCKET_PATH)?;
        Ok(())
    }

    /// Start the client.
    ///
    /// Spawns the background thread and returns immediately; the thread
    /// retries connecting to the daemon until it succeeds.  The virtual
    /// keyboard is created with the given identity.  Use
    /// [`KarabinerClient::wait_ready`] to block until the virtual keyboard
    /// is ready.
    pub fn connect(
        identity: KeyboardIdentity,
    ) -> Result<Self, KarabinerClientError> {
        let (tx, rx) = mpsc::channel();
        let ready = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));

        // The thread gets its own clones; the struct keeps the originals.
        let ready_thread = Arc::clone(&ready);
        let shutdown_thread = Arc::clone(&shutdown);

        thread::Builder::new()
            .name("karabiner-client".into())
            .spawn(move || {
                client_loop(rx, ready_thread, shutdown_thread, identity)
            })
            .map_err(KarabinerClientError::ThreadSpawnFailed)?;

        Ok(Self {
            tx,
            ready,
            shutdown,
        })
    }

    /// Whether the daemon has reported the virtual keyboard as ready.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Block until the virtual keyboard is ready or `timeout` elapses.
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.ready.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(50));
        }
        true
    }

    /// Enqueue a `keyboard_input` report.
    ///
    /// `usages` holds up to 32 simultaneous keys as HID Keyboard Page
    /// usages.  The report is written by the background thread as soon as it
    /// is picked up (within ~50 ms) and the virtual keyboard is ready.
    /// Reports enqueued while the keyboard is not ready are dropped so that
    /// stale key state is never replayed after a reconnect.
    pub fn send_keyboard_report(
        &self,
        modifiers: u8,
        usages: &[u16],
    ) -> Result<(), KarabinerClientError> {
        self.tx
            .send(ClientCommand::KeyboardReport {
                modifiers,
                usages: usages.to_vec(),
            })
            .map_err(|_| KarabinerClientError::ClientDisconnected)
    }

    /// Enqueue a `consumer_input` report that presses the given Consumer
    /// Page usage.
    pub fn send_consumer_report(
        &self,
        usage: u16,
    ) -> Result<(), KarabinerClientError> {
        self.tx
            .send(ClientCommand::ConsumerPress { usage })
            .map_err(|_| KarabinerClientError::ClientDisconnected)
    }

    /// Enqueue an all-clear `consumer_input` report that releases any held
    /// consumer key.
    pub fn send_consumer_release(&self) -> Result<(), KarabinerClientError> {
        self.tx
            .send(ClientCommand::ConsumerRelease)
            .map_err(|_| KarabinerClientError::ClientDisconnected)
    }
}

impl Drop for KarabinerClient {
    fn drop(&mut self) {
        // Ask the background thread to exit; it stops within one poll or
        // reconnect cycle.
        self.shutdown.store(true, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Background thread
// ---------------------------------------------------------------------------

/// The background thread's main loop: connect, run the connection, and retry
/// until shutdown is requested.
fn client_loop(
    rx: mpsc::Receiver<ClientCommand>,
    ready: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    identity: KeyboardIdentity,
) {
    loop {
        if shutdown.load(Ordering::Acquire) {
            return;
        }

        match UnixStream::connect(SOCKET_PATH) {
            Ok(stream) => {
                eprintln!("Karabiner daemon connected");
                if let Err(e) =
                    run_connection(stream, &rx, &ready, &shutdown, identity)
                {
                    eprintln!(
                        "Karabiner daemon connection lost ({e}); \
                         reconnecting in {} ms",
                        RECONNECT_INTERVAL.as_millis()
                    );
                }
                ready.store(false, Ordering::Release);
            }
            Err(e) => {
                eprintln!(
                    "Karabiner daemon not reachable ({e}); retrying in {} ms",
                    RECONNECT_INTERVAL.as_millis()
                );
            }
        }

        if shutdown.load(Ordering::Acquire) {
            return;
        }
        thread::sleep(RECONNECT_INTERVAL);
    }
}

/// Run a single connection: initialize the virtual keyboard, then process
/// frames until the connection is lost or shutdown is requested.
fn run_connection(
    mut stream: UnixStream,
    rx: &mpsc::Receiver<ClientCommand>,
    ready: &Arc<AtomicBool>,
    shutdown: &Arc<AtomicBool>,
    identity: KeyboardIdentity,
) -> std::io::Result<()> {
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;

    let mut next_request_id: u64 = 1;

    // Create the virtual keyboard device node.  The node exists only while
    // a client is connected and is destroyed on disconnect.
    send_request(
        &mut stream,
        &mut next_request_id,
        REQ_KEYBOARD_INITIALIZE,
        &build_keyboard_parameters(&identity),
    )?;

    let mut last_heartbeat = Instant::now();
    let mut last_received = Instant::now();

    loop {
        if shutdown.load(Ordering::Acquire) {
            return Ok(());
        }

        let now = Instant::now();

        // Send a heartbeat if one is due.
        if now.duration_since(last_heartbeat) >= HEARTBEAT_INTERVAL {
            stream.write_all(&encode_frame(MSG_HEARTBEAT, &[]))?;
            last_heartbeat = Instant::now();
        }

        // Drop the connection if no frame has been received within
        // READ_TIMEOUT.
        if now.duration_since(last_received) >= READ_TIMEOUT {
            return Err(std::io::Error::new(
                ErrorKind::TimedOut,
                "no frame received within the read timeout",
            ));
        }

        // Poll the socket with a short timeout so that enqueued reports are
        // written promptly.
        stream.set_read_timeout(Some(POLL_INTERVAL))?;
        match decode_frame(&mut stream) {
            Ok(frame) => {
                last_received = Instant::now();
                handle_frame(&mut stream, ready, frame)?;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }

        // Send pending reports.  While the keyboard is not ready they are
        // dropped so that stale key state is never replayed after a
        // reconnect.
        while let Ok(cmd) = rx.try_recv() {
            if ready.load(Ordering::Acquire) {
                send_command(&mut stream, &mut next_request_id, cmd)?;
            }
        }
    }
}

/// Handle a frame received from the daemon.
fn handle_frame(
    stream: &mut UnixStream,
    ready: &Arc<AtomicBool>,
    frame: Frame,
) -> std::io::Result<()> {
    match frame.msg_type {
        // The daemon pushes state updates as request frames; each one must
        // be answered with an empty response frame or the daemon stalls its
        // state reporting.
        MSG_REQUEST => {
            let (request_id, payload) = split_id_body(&frame.body)?;
            apply_state_pairs(ready, payload);
            stream.write_all(&encode_id_frame(
                MSG_RESPONSE,
                request_id,
                &[],
            ))?;
        }
        // Responses to our own requests may carry state pairs as well.
        MSG_RESPONSE => {
            let (_request_id, payload) = split_id_body(&frame.body)?;
            apply_state_pairs(ready, payload);
        }
        // Heartbeats and other message types need no action.
        _ => {}
    }
    Ok(())
}

/// Apply the daemon's state-update pairs to the readiness flag.
///
/// The payload is a sequence of `[response_type][value]` pairs.
fn apply_state_pairs(ready: &Arc<AtomicBool>, payload: &[u8]) {
    for pair in payload.as_chunks::<2>().0 {
        match (pair[0], pair[1]) {
            (RESP_DRIVER_ACTIVATED, 1) => {
                eprintln!("Karabiner driver activated");
            }
            (RESP_DRIVER_CONNECTED, 1) => {
                eprintln!("Karabiner driver connected");
            }
            (RESP_DRIVER_VERSION_MISMATCHED, 1) => {
                eprintln!("Karabiner driver version mismatched");
            }
            (RESP_VIRTUAL_HID_KEYBOARD_READY, value) => {
                ready.store(value == 1, Ordering::Release);
                if value == 1 {
                    eprintln!("Karabiner virtual keyboard ready");
                }
            }
            _ => {}
        }
    }
}

/// Send a request frame with the given request type and report bytes.
fn send_request(
    stream: &mut UnixStream,
    next_request_id: &mut u64,
    request_type: u8,
    report: &[u8],
) -> std::io::Result<()> {
    let payload = build_request_payload(request_type, report);
    let frame = encode_id_frame(MSG_REQUEST, *next_request_id, &payload);
    *next_request_id = next_request_id.wrapping_add(1);
    stream.write_all(&frame)
}

/// Send an enqueued command as a request frame.
fn send_command(
    stream: &mut UnixStream,
    next_request_id: &mut u64,
    cmd: ClientCommand,
) -> std::io::Result<()> {
    match cmd {
        ClientCommand::KeyboardReport { modifiers, usages } => {
            let report = build_keyboard_input_report(modifiers, &usages);
            send_request(
                stream,
                next_request_id,
                REQ_POST_KEYBOARD_INPUT_REPORT,
                &report,
            )
        }
        ClientCommand::ConsumerPress { usage } => {
            let report = build_consumer_input_report(usage);
            send_request(
                stream,
                next_request_id,
                REQ_POST_CONSUMER_INPUT_REPORT,
                &report,
            )
        }
        ClientCommand::ConsumerRelease => {
            let report = build_consumer_release_report();
            send_request(
                stream,
                next_request_id,
                REQ_POST_CONSUMER_INPUT_REPORT,
                &report,
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// The expected `virtual_hid_keyboard_initialize` frame.  The Phase-0
    /// wire capture omitted the 24-byte `virtual_hid_keyboard_parameters`
    /// payload, which service 8.2.0 rejects with a "buffer size error"; this
    /// is the corrected layout with vendor ID `0x16c0`, product ID `0x27db`,
    /// and country code `0`.
    const EXPECTED_INITIALIZE_FRAME: [u8; 40] = [
        0x00, 0x00, 0x00, 0x24, // body size = 36
        0x04, // request
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // request ID = 1
        0x07, 0x00, // protocol version 7 (LE)
        0x00, // virtual_hid_keyboard_initialize
        0xc0, 0x16, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, // vendor ID 0x16c0 (LE)
        0xdb, 0x27, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, // product ID 0x27db (LE)
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, // country code 0 (LE)
    ];

    /// The exact `post_keyboard_input_report` frame captured on the wire in
    /// Phase 0: left shift + 'e' (usage 0x000E).
    fn captured_keyboard_frame() -> Vec<u8> {
        let mut frame = vec![
            0x00, 0x00, 0x00, 0x4f, // body size = 79
            0x04, // request
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x02, // request ID = 2
            0x07, 0x00, // protocol version 7 (LE)
            0x06, // post_keyboard_input_report
            0x01, // report ID
            0x02, // modifiers: left shift
            0x00, // reserved
            0x0e, 0x00, // usage 0x000E ('e') in the first slot
        ];
        frame.extend_from_slice(&[0u8; 62]); // the remaining 31 key slots
        frame
    }

    /// Poll `condition` until it holds or the timeout elapses.
    fn wait_until(condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            if Instant::now() >= deadline {
                panic!("condition not met within 5 s");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn test_encode_heartbeat_frame() {
        // The exact heartbeat frame captured on the wire in Phase 0: body
        // size 1, message type `heartbeat`, no payload.
        assert_eq!(encode_frame(MSG_HEARTBEAT, &[]), [0, 0, 0, 1, 0]);
    }

    #[test]
    fn test_encode_initialize_frame_matches_expected() {
        let payload = build_request_payload(
            REQ_KEYBOARD_INITIALIZE,
            &build_keyboard_parameters(&OUTPUT_KEYBOARD_IDENTITY),
        );
        assert_eq!(
            encode_id_frame(MSG_REQUEST, 1, &payload),
            EXPECTED_INITIALIZE_FRAME
        );
    }

    #[test]
    fn test_keyboard_parameters_size() {
        // The daemon requires exactly 24 bytes after the protocol version
        // and request type, or it rejects the initialize request.
        assert_eq!(
            build_keyboard_parameters(&OUTPUT_KEYBOARD_IDENTITY).len(),
            24
        );
    }

    #[test]
    fn test_build_keyboard_parameters_injection_identity() {
        // The injection keyboard must register with its own product ID so
        // the daemon can distinguish it from the output keyboard.
        assert_eq!(
            build_keyboard_parameters(&INJECTION_KEYBOARD_IDENTITY),
            [
                0xc0, 0x16, 0, 0, 0, 0, 0, 0, // vendor ID 0x16c0 (LE)
                0xdc, 0x27, 0, 0, 0, 0, 0, 0, // product ID 0x27dc (LE)
                0, 0, 0, 0, 0, 0, 0, 0, // country code 0 (LE)
            ]
        );
    }

    #[test]
    fn test_encode_keyboard_frame_matches_capture() {
        let report = build_keyboard_input_report(0x02, &[0x0E]);
        let payload =
            build_request_payload(REQ_POST_KEYBOARD_INPUT_REPORT, &report);
        assert_eq!(
            encode_id_frame(MSG_REQUEST, 2, &payload),
            captured_keyboard_frame()
        );
    }

    #[test]
    fn test_decode_frame_round_trip() {
        let payload =
            build_request_payload(REQ_POST_KEYBOARD_INPUT_REPORT, &[0xAA]);
        let frame = encode_id_frame(MSG_REQUEST, 42, &payload);
        let decoded = decode_frame(&mut Cursor::new(frame)).unwrap();
        assert_eq!(decoded.msg_type, MSG_REQUEST);
        assert_eq!(&decoded.body[..8], &42u64.to_be_bytes());
        assert_eq!(&decoded.body[8..], &payload);
    }

    #[test]
    fn test_decode_frame_rejects_zero_body() {
        let frame = [0u8, 0, 0, 0, 0]; // body size 0
        let err = decode_frame(&mut Cursor::new(frame)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn test_decode_frame_rejects_oversized_body() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&((MAX_FRAME_SIZE + 1) as u32).to_be_bytes());
        frame.push(MSG_HEARTBEAT);
        let err = decode_frame(&mut Cursor::new(frame)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn test_request_payload_layout() {
        let payload =
            build_request_payload(REQ_POST_KEYBOARD_INPUT_REPORT, &[0xAA]);
        assert_eq!(
            &payload[..3],
            &[0x07, 0x00, REQ_POST_KEYBOARD_INPUT_REPORT]
        );
        assert_eq!(&payload[3..], &[0xAA]);
    }

    #[test]
    fn test_build_keyboard_input_report_single_key() {
        let report = build_keyboard_input_report(0x02, &[0x0E]);
        assert_eq!(report.len(), KEYBOARD_REPORT_SIZE);
        assert_eq!(report[0], KEYBOARD_REPORT_ID);
        assert_eq!(report[1], 0x02); // left shift
        assert_eq!(report[2], 0); // reserved
        assert_eq!(&report[3..5], &[0x0E, 0x00]); // 'e' in the first slot
        assert!(report[5..].iter().all(|&b| b == 0));
    }

    #[test]
    fn test_build_keyboard_input_report_multiple_keys() {
        let report = build_keyboard_input_report(0x00, &[0x04, 0x05, 0x2C]);
        assert_eq!(&report[3..9], &[0x04, 0x00, 0x05, 0x00, 0x2C, 0x00]);
        assert!(report[9..].iter().all(|&b| b == 0));
    }

    #[test]
    fn test_build_keyboard_input_report_truncates_at_32_keys() {
        let usages: Vec<u16> = (0..=32).collect(); // 33 usages
        let report = build_keyboard_input_report(0x00, &usages);
        assert_eq!(report.len(), KEYBOARD_REPORT_SIZE);
        // The last slot holds the 32nd usage (value 31).
        assert_eq!(&report[65..67], &31u16.to_le_bytes());
    }

    #[test]
    fn test_build_keyboard_input_report_empty() {
        let report = build_keyboard_input_report(0x00, &[]);
        assert_eq!(report[0], KEYBOARD_REPORT_ID);
        assert!(report[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn test_build_consumer_input_report() {
        let mut expected = [0u8; CONSUMER_REPORT_SIZE];
        expected[0] = CONSUMER_REPORT_ID;
        expected[1..3].copy_from_slice(&0xCDu16.to_le_bytes());
        assert_eq!(build_consumer_input_report(0xCD), expected);
    }

    #[test]
    fn test_build_consumer_input_report_high_usage() {
        let mut expected = [0u8; CONSUMER_REPORT_SIZE];
        expected[0] = CONSUMER_REPORT_ID;
        expected[1..3].copy_from_slice(&0x1234u16.to_le_bytes());
        assert_eq!(build_consumer_input_report(0x1234), expected);
    }

    #[test]
    fn test_build_consumer_release_report() {
        let mut expected = [0u8; CONSUMER_REPORT_SIZE];
        expected[0] = CONSUMER_REPORT_ID;
        assert_eq!(build_consumer_release_report(), expected);
    }

    /// Run the connection loop against a mock daemon on the other end of a
    /// socket pair and verify the full handshake: initialize, state push +
    /// empty response, readiness, and report framing.
    #[test]
    fn test_run_connection_against_mock_daemon() {
        let (client_stream, mut daemon_stream) = UnixStream::pair().unwrap();
        // Generous timeouts so the test fails loudly instead of hanging.
        daemon_stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let (tx, rx) = mpsc::channel::<ClientCommand>();
        let ready = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));

        // The thread gets its own clone; the test keeps `ready` to poll.
        let ready_thread = Arc::clone(&ready);

        let client = thread::spawn(move || {
            run_connection(
                client_stream,
                &rx,
                &ready_thread,
                &shutdown,
                OUTPUT_KEYBOARD_IDENTITY,
            )
        });

        // 1. The client sends virtual_hid_keyboard_initialize first.
        let frame = decode_frame(&mut daemon_stream).unwrap();
        assert_eq!(frame.msg_type, MSG_REQUEST);
        let (request_id, payload) = split_id_body(&frame.body).unwrap();
        assert_eq!(request_id, 1);
        // The payload carries the protocol version, the request type, and
        // the 24-byte keyboard parameters (see EXPECTED_INITIALIZE_FRAME).
        assert_eq!(payload, &EXPECTED_INITIALIZE_FRAME[13..]);

        // 2. The daemon pushes a state update (keyboard ready) as a request
        //    frame; the client must answer with an empty response.
        let state_payload = [RESP_VIRTUAL_HID_KEYBOARD_READY, 1];
        daemon_stream
            .write_all(&encode_id_frame(MSG_REQUEST, 100, &state_payload))
            .unwrap();

        let frame = decode_frame(&mut daemon_stream).unwrap();
        assert_eq!(frame.msg_type, MSG_RESPONSE);
        let (response_id, response_payload) =
            split_id_body(&frame.body).unwrap();
        assert_eq!(response_id, 100);
        assert!(response_payload.is_empty());

        // The client marks the keyboard ready.
        wait_until(|| ready.load(Ordering::Acquire));

        // 3. A keyboard report command is framed as a request carrying the
        //    67-byte keyboard_input report.
        tx.send(ClientCommand::KeyboardReport {
            modifiers: 0x02,
            usages: vec![0x0E],
        })
        .unwrap();

        let frame = decode_frame(&mut daemon_stream).unwrap();
        assert_eq!(frame.msg_type, MSG_REQUEST);
        let (request_id, payload) = split_id_body(&frame.body).unwrap();
        assert_eq!(request_id, 2); // initialize was request ID 1
        assert_eq!(
            &payload[..3],
            &[0x07, 0x00, REQ_POST_KEYBOARD_INPUT_REPORT]
        );
        let report = &payload[3..];
        assert_eq!(report.len(), KEYBOARD_REPORT_SIZE);
        assert_eq!(report[0], KEYBOARD_REPORT_ID);
        assert_eq!(report[1], 0x02); // left shift
        assert_eq!(&report[3..5], &[0x0E, 0x00]); // 'e' in the first slot

        // 4. A consumer press is framed with the 65-byte consumer_input report
        //    (report ID + 32 usage slots).
        tx.send(ClientCommand::ConsumerPress { usage: 0xCD })
            .unwrap();

        let frame = decode_frame(&mut daemon_stream).unwrap();
        assert_eq!(frame.msg_type, MSG_REQUEST);
        let (request_id, payload) = split_id_body(&frame.body).unwrap();
        assert_eq!(request_id, 3);
        assert_eq!(
            &payload[..3],
            &[0x07, 0x00, REQ_POST_CONSUMER_INPUT_REPORT]
        );
        let report = &payload[3..];
        assert_eq!(report.len(), CONSUMER_REPORT_SIZE);
        assert_eq!(report[0], CONSUMER_REPORT_ID);
        assert_eq!(&report[1..3], &0xCDu16.to_le_bytes());
        assert!(report[3..].iter().all(|&b| b == 0));

        // 5. Closing the daemon end terminates the connection loop.
        drop(daemon_stream);
        let result = client.join().unwrap();
        assert!(result.is_err()); // EOF from the closed daemon end
    }
}
