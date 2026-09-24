// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The Windows control endpoint: a byte-mode named pipe.
//!
//! The daemon creates `\\.\\pipe\\keymapperd` and serves one command per
//! connection. The pipe is the auth mechanism: its DACL grants only the
//! current user (a single `ACCESS_ALLOWED` ACE for the caller's SID) full
//! control, so only that user can open it. This supersedes the
//! `daemon_token` removed in Phase 2.
//!
//! The pipe is created *without* a security descriptor, and the owner-only
//! DACL is applied right afterwards with `SetSecurityInfo`. Passing the
//! descriptor to `CreateNamedPipeW` directly fails on recent Windows builds
//! (the creation is rejected with `ERROR_LOCAL_DEVICE_NOT_FOUND` or
//! `ERROR_INVALID_SECURITY_DESCRIPTOR`), while the two-step approach
//! succeeds. The ACL itself is filled in byte by byte, because
//! advapi32's `InitializeAcl`/`AddAccessAllowedAce` write an `AclSize` that
//! does not match the ACE they add, which stricter validation rejects.

use std::{
    io::{Read, Write},
    time::Duration,
};

use log::{debug, info, warn};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING,
            ERROR_PIPE_CONNECTED, GENERIC_ALL, GetLastError, HANDLE,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::{
            ACL_REVISION, GetLengthSid, GetTokenInformation,
            InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR, PSID,
            SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl, TOKEN_QUERY,
            TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
            FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING,
            PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
        },
        System::{
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe,
                NAMED_PIPE_MODE, PIPE_READMODE_BYTE,
                PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, WaitNamedPipeW,
            },
            Threading::{
                CreateEventW, GetCurrentProcess, OpenProcessToken, ResetEvent,
                WaitForSingleObject,
            },
        },
    },
    core::PCWSTR,
};

use super::{IoStream, handle_connection};

/// The control pipe name.
const PIPE_NAME: &str = r"\\.\pipe\keymapperd";

/// The pipe's input and output buffer sizes (bytes).
const PIPE_BUFFER: u32 = 4096;

/// The `SECURITY_DESCRIPTOR_REVISION` value (kept local to avoid pulling in an
/// unrelated feature just for the named constant).
const SD_REVISION: u32 = 1;

/// Size of the on-wire `ACL` header: revision, size, count, and two unused
/// bytes.
const ACL_HEADER_BYTES: usize = 8;

/// Size of an `ACCESS_ALLOWED` ACE's fixed part (type, flags, size, mask);
/// the SID bytes follow directly.
const ACE_FIXED_BYTES: usize = 8;

/// `SE_FILE_OBJECT`, the object type of a named pipe for `SetSecurityInfo`
/// (not in the `windows` crate's surface, kept local like `SD_REVISION`).
const SE_FILE_OBJECT: u32 = 1;

/// `DACL_SECURITY_INFORMATION`: apply only the DACL.
const DACL_SECURITY_INFORMATION: u32 = 4;

/// The `ERROR_FILE_NOT_FOUND` code: no pipe to open means no daemon.
const ERROR_FILE_NOT_FOUND: i32 = 2;

/// The `ERROR_PIPE_BUSY` code: the daemon exists but is serving a client.
const ERROR_PIPE_BUSY: i32 = 231;

/// How long to keep waiting for the daemon's single pipe instance to be
/// released: on the client side when opening the pipe, and on the daemon side
/// when creating it after a previous daemon was killed mid-connection. The
/// serve loop disconnects within microseconds of the previous client's close,
/// so this only matters under load or for a misbehaving peer that never
/// closes; 2 s is a comfortable upper bound.
const BUSY_RETRY_ATTEMPTS: u32 = 20;
const BUSY_RETRY_WAIT_MS: u32 = 100;

/// Upper bound on every blocking wait in the serve loop: waiting for a
/// client to connect, reading the request, writing the reply, and waiting
/// for the client to close after an exchange.
///
/// Without it, a peer that connects and sends nothing — or reads the
/// reply but never closes its handle — holds the single pipe instance
/// until it exits, wedging the control endpoint. Generous for a local
/// CLI, which completes an exchange in milliseconds, while keeping the
/// wedge time bounded.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// A named-pipe handle owned by the control-socket thread.
///
/// `HANDLE` is a raw pointer and therefore neither `Send` nor `Sync`, but the
/// pipe is created on the caller thread and served exclusively by the
/// `control-socket` thread, so moving it across the thread boundary is safe.
///
/// Copying it only duplicates the pointer value (the same OS handle is reused
/// for every connection), so it can be `Copy`.
#[derive(Clone, Copy)]
struct PipeHandle(HANDLE);

// SAFETY: each `PipeHandle` is moved into the control-socket thread and never
// shared between threads again.
unsafe impl Send for PipeHandle {}

/// A named-pipe handle as a `Read`/`Write` stream (server side).
///
/// The server pipe is created with `FILE_FLAG_OVERLAPPED`, so
/// `ReadFile`/`WriteFile` are issued asynchronously and awaited with a
/// bounded wait (see [`wait_overlapped`]); a peer close still surfaces as
/// `ERROR_BROKEN_PIPE` (mapped to EOF) or an I/O error. The *event* is
/// shared with the accept wait and reset before each request, since the
/// serve loop runs one operation at a time.
struct Pipe {
    handle: PipeHandle,
    event: HANDLE,
}

/// A client-side pipe connection.
///
/// The client opens the pipe synchronously (no `FILE_FLAG_OVERLAPPED`),
/// so its reads and blocks on the exchange itself are bounded by the
/// daemon's own [`IO_TIMEOUT`]: the daemon always answers or closes the
/// instance within one timeout.
///
/// Closes its handle on drop. The daemon serves one connection at a time on a
/// single pipe instance and releases the instance only after the client end
/// closes (or the daemon's close-wait times out, see
/// `wait_for_client_close`), so a handle that outlived the
/// exchange would keep the instance connected and every later `CreateFileW`
/// would fail with `ERROR_PIPE_BUSY` until the client process exits.
struct ClientPipe {
    handle: PipeHandle,
}

impl Read for ClientPipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut bytes_read = 0u32;
        match unsafe {
            ReadFile(
                self.handle.0,
                Some(buf),
                Some(&mut bytes_read as *mut _),
                None,
            )
        } {
            Ok(()) => Ok(bytes_read as usize),
            // The peer closed the pipe; report EOF as a clean connection end.
            Err(_) if unsafe { GetLastError() } == ERROR_BROKEN_PIPE => Ok(0),
            Err(_) => Err(std::io::Error::last_os_error()),
        }
    }
}

impl Write for ClientPipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut bytes_written = 0u32;
        match unsafe {
            WriteFile(
                self.handle.0,
                Some(buf),
                Some(&mut bytes_written as *mut _),
                None,
            )
        } {
            Ok(()) => Ok(bytes_written as usize),
            Err(_) => Err(std::io::Error::last_os_error()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // Byte-mode pipe writes are synchronous; there is nothing to flush.
        Ok(())
    }
}

impl Drop for ClientPipe {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle.0);
        }
    }
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut ov = OVERLAPPED {
            hEvent: self.event,
            ..Default::default()
        };
        // The event is reused across operations; a stale signal from a
        // previous completion would make the wait below return too early.
        let _ = unsafe { ResetEvent(self.event) };
        match unsafe {
            ReadFile(self.handle.0, Some(buf), None, Some(&raw mut ov))
        } {
            Ok(()) => {}
            // The request was queued; the wait picks up the result.
            Err(_) if unsafe { GetLastError() } == ERROR_IO_PENDING => {}
            // The peer closed the pipe; report EOF as a clean connection end.
            Err(_) if unsafe { GetLastError() } == ERROR_BROKEN_PIPE => {
                return Ok(0);
            }
            Err(_) => return Err(std::io::Error::last_os_error()),
        }
        match wait_overlapped(self.handle.0, &ov) {
            Ok(bytes) => Ok(bytes as usize),
            // A peer close while the read was pending surfaces here.
            Err(_) if unsafe { GetLastError() } == ERROR_BROKEN_PIPE => Ok(0),
            Err(e) => Err(e),
        }
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut ov = OVERLAPPED {
            hEvent: self.event,
            ..Default::default()
        };
        let _ = unsafe { ResetEvent(self.event) };
        match unsafe {
            WriteFile(self.handle.0, Some(buf), None, Some(&raw mut ov))
        } {
            Ok(()) => {}
            Err(_) if unsafe { GetLastError() } == ERROR_IO_PENDING => {}
            Err(_) => return Err(std::io::Error::last_os_error()),
        }
        wait_overlapped(self.handle.0, &ov).map(|bytes| bytes as usize)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // Byte-mode pipe writes complete in full; there is nothing to flush.
        Ok(())
    }
}

/// Wait for a pending overlapped request on *handle* to complete and
/// return the number of bytes transferred.
///
/// The wait is bounded by [`IO_TIMEOUT`]. On timeout the request is
/// cancelled and the cancellation is reaped, so the kernel does not
/// touch *ov* or its event after this function returns. A timeout is
/// reported as [`std::io::ErrorKind::TimedOut`], which callers treat as
/// a connection end.
fn wait_overlapped(handle: HANDLE, ov: &OVERLAPPED) -> std::io::Result<u32> {
    let timeout_ms = u32::try_from(IO_TIMEOUT.as_millis()).unwrap_or(u32::MAX);
    match unsafe { WaitForSingleObject(ov.hEvent, timeout_ms) } {
        WAIT_OBJECT_0 => {
            let mut transferred = 0u32;
            // On failure the last error carries the operation's real
            // failure code (e.g. a broken pipe from a peer close), which
            // callers inspect.
            unsafe {
                GetOverlappedResult(handle, ov, &mut transferred, false)
            }
            .map_err(|_| std::io::Error::last_os_error())?;
            Ok(transferred)
        }
        WAIT_TIMEOUT => {
            unsafe {
                let _ = CancelIoEx(handle, Some(ov as *const OVERLAPPED));
                // Reap the cancelled request so `ov` and its event are
                // released by the kernel before this function returns.
                let mut transferred = 0u32;
                let _ =
                    GetOverlappedResult(handle, ov, &mut transferred, true);
            }
            Err(std::io::Error::from(std::io::ErrorKind::TimedOut))
        }
        // The wait itself failed (e.g. an invalid event handle); surface
        // the OS error so the caller tears the connection down.
        _ => Err(std::io::Error::last_os_error()),
    }
}

/// An owner-only security descriptor, keeping the backing `ACL` buffer and the
/// An owner-only security descriptor, keeping the backing `ACL` buffer and
/// the `SECURITY_DESCRIPTOR` alive together. The descriptor's DACL points
/// into `acl`.
struct OwnerOnlyDescriptor {
    sd: SECURITY_DESCRIPTOR,
    /// Never read by Rust; the field only keeps the buffer alive because the
    /// descriptor's DACL points into it.
    #[allow(dead_code)]
    acl: Vec<u8>,
}

/// `SetSecurityInfo` (advapi32), which the `windows` crate does not expose.
mod ffi {
    // SAFETY: FFI wrapper around the stable advapi32 `SetSecurityInfo`
    // entry point; parameters are passed by value or pointer and the return
    // value is a `BOOL`.
    unsafe extern "system" {
        pub fn SetSecurityInfo(
            hobject: *mut core::ffi::c_void,
            object_type: u32,
            security_information: u32,
            security_descriptor: *mut core::ffi::c_void,
        ) -> i32;
    }
}

/// Apply *sd* as the DACL of *handle*. Returns the Win32 error, or 0.
fn set_dacl(handle: HANDLE, sd: &SECURITY_DESCRIPTOR) -> u32 {
    let ok = unsafe {
        ffi::SetSecurityInfo(
            handle.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            sd as *const _ as *mut core::ffi::c_void,
        )
    };
    if ok != 0 {
        0
    } else {
        unsafe { GetLastError().0 }
    }
}

/// The caller's primary SID, from the current process token, owned by the
/// caller as a byte buffer.
///
/// `GetTokenInformation` writes the SID *inside* the caller's buffer (the
/// `TOKEN_USER` pointer lands just past its 8-byte header), so the bytes are
/// copied out into a `Vec<u8>`. The SID must never be passed to `FreeSid`: it
/// is not a system-allocated SID object, and freeing a mid-allocation pointer
/// corrupts the heap.
fn current_user_sid() -> Option<Vec<u8>> {
    unsafe {
        let process = GetCurrentProcess();
        let mut token = HANDLE::default();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token as *mut _)
            .is_err()
        {
            return None;
        }

        // Two-call `GetTokenInformation`: first size, then fill.
        let mut needed = 0u32;
        let _ = GetTokenInformation(
            token,
            TokenUser,
            None,
            0,
            &mut needed as *mut _,
        );
        if needed == 0 {
            let _ = CloseHandle(token);
            return None;
        }
        let mut buffer = vec![0u8; needed as usize];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr() as *mut _),
            needed,
            &mut needed as *mut _,
        );
        let _ = CloseHandle(token);
        if ok.is_err() {
            return None;
        }

        // Copy the SID bytes out of the scratch buffer before it drops.
        let token_user: TOKEN_USER =
            core::ptr::read_unaligned(buffer.as_ptr() as *const TOKEN_USER);
        let sid_ptr = token_user.User.Sid.0;
        if sid_ptr.is_null() {
            return None;
        }
        let sid_len = GetLengthSid(PSID(sid_ptr)) as usize;
        Some(
            std::slice::from_raw_parts(sid_ptr as *const u8, sid_len).to_vec(),
        )
    }
}

/// Build an owner-only descriptor granting the SID in *sid* full control of
/// the pipe.
///
/// The ACL is filled in byte by byte (8-byte header + one
/// `ACCESS_ALLOWED` ACE + the SID): advapi32's `InitializeAcl`/
/// `AddAccessAllowedAce` leave the `AclSize` field inconsistent with the
/// ACE they append, and creation with such a descriptor is rejected on
/// recent Windows builds.
///
/// Returns `None` when the descriptor cannot be built (in which case the
/// pipe is not exposed at all, so it is never weaker than owner-only).
fn owner_only_descriptor(sid: &[u8]) -> Option<OwnerOnlyDescriptor> {
    let total = ACL_HEADER_BYTES + ACE_FIXED_BYTES + sid.len();
    let mut acl = vec![0u8; total];
    // Header: revision, total size, one ACE.
    acl[0] = ACL_REVISION.0 as u8;
    acl[2..4].copy_from_slice(&(total as u16).to_le_bytes());
    acl[4..6].copy_from_slice(&1u16.to_le_bytes());
    // ACE at offset 8: type 0 (`ACCESS_ALLOWED_ACE_TYPE`), no flags, its own
    // size (fixed part + SID), the mask, then the SID bytes.
    acl[8] = 0;
    let ace_size = ACE_FIXED_BYTES + sid.len();
    acl[10..12].copy_from_slice(&(ace_size as u16).to_le_bytes());
    acl[12..16].copy_from_slice(&GENERIC_ALL.0.to_le_bytes());
    acl[16..].copy_from_slice(sid);

    // `mem::zeroed` on a struct containing raw pointers is unsafe on Rust
    // 2024; the descriptor is fully initialised by the calls below.
    let sd: SECURITY_DESCRIPTOR = unsafe { core::mem::zeroed() };
    // Two-step cast: a reference may only become a raw pointer of its own type
    // in a single step, so route through `*mut SECURITY_DESCRIPTOR` first.
    let sd_ptr = PSECURITY_DESCRIPTOR(
        &sd as *const SECURITY_DESCRIPTOR as *mut core::ffi::c_void,
    );
    if unsafe { InitializeSecurityDescriptor(sd_ptr, SD_REVISION) }.is_err() {
        return None;
    }
    if unsafe {
        SetSecurityDescriptorDacl(
            sd_ptr,
            true,
            Some(acl.as_ptr() as *const _),
            false,
        )
    }
    .is_err()
    {
        return None;
    }

    Some(OwnerOnlyDescriptor { sd, acl })
}

/// The NUL-terminated UTF-16 form of *name*, for the `W` Win32 APIs.
fn to_wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Create the pipe and spawn the serve thread.
pub fn start() {
    start_with_name(PIPE_NAME)
}

/// Create the pipe at *name* and spawn the serve thread.
fn start_with_name(name: &str) {
    let Some(sid) = current_user_sid() else {
        warn!(
            "Could not resolve the current user's SID; runtime log-level \
             control is disabled"
        );
        return;
    };
    let Some(desc) = owner_only_descriptor(&sid) else {
        warn!(
            "Could not build an owner-only pipe security descriptor; runtime \
             log-level control is disabled"
        );
        return;
    };
    // The ACE copied the SID bytes into the ACL, so the owned buffer only
    // needs to drop when this function returns (never `FreeSid`; see
    // `current_user_sid`). `desc` stays alive across the `SetSecurityInfo`
    // call below, which copies the DACL into the pipe object.

    let wide_name = to_wide(name);
    // The pipe is created without a security descriptor; the owner-only DACL
    // is applied right afterwards. Passing the descriptor to
    // `CreateNamedPipeW` directly is rejected on recent Windows builds.
    // With a single pipe instance, a daemon killed mid-connection (e.g. by
    // the e2e harness's `TerminateProcess`) can leave the name briefly busy,
    // so the creation is retried with `WaitNamedPipeW`, mirroring the
    // client-side busy handling in `connect_to`.
    let (pipe, code) = {
        let mut attempts = 0;
        loop {
            let pipe = unsafe {
                CreateNamedPipeW(
                    PCWSTR(wide_name.as_ptr()),
                    FILE_FLAGS_AND_ATTRIBUTES(
                        PIPE_ACCESS_DUPLEX.0
                            | FILE_FLAG_FIRST_PIPE_INSTANCE.0
                            | FILE_FLAG_OVERLAPPED.0,
                    ),
                    NAMED_PIPE_MODE(
                        PIPE_TYPE_BYTE.0
                            | PIPE_READMODE_BYTE.0
                            | PIPE_REJECT_REMOTE_CLIENTS.0,
                    ),
                    1, /* one instance: one client at a time, matching the
                        * unix path */
                    PIPE_BUFFER,
                    PIPE_BUFFER,
                    0,
                    None,
                )
            };
            let code = unsafe { GetLastError() };
            if !pipe.is_invalid()
                || code.0 != ERROR_PIPE_BUSY as u32
                || attempts + 1 >= BUSY_RETRY_ATTEMPTS
            {
                break (pipe, code);
            }
            attempts += 1;
            // Wait for the name to become openable again; the wait times out
            // after each attempt, so the loop is bounded by
            // `BUSY_RETRY_ATTEMPTS` (~2 s in total).
            let _ = unsafe {
                WaitNamedPipeW(PCWSTR(wide_name.as_ptr()), BUSY_RETRY_WAIT_MS)
            };
        }
    };

    if pipe.is_invalid() {
        warn!(
            "Could not create the control pipe {name} (error {:#010x}); \
             runtime log-level control is disabled",
            code.0
        );
        return;
    }

    // Apply the owner-only DACL. Fail closed: a pipe that cannot be locked
    // down to the owner is not exposed at all.
    let dacl_error = set_dacl(pipe, &desc.sd);
    drop(desc);
    if dacl_error != 0 {
        unsafe {
            let _ = CloseHandle(pipe);
        }
        warn!(
            "Could not apply the owner-only DACL to the control pipe {name} \
             (error {dacl_error:#010x}); runtime log-level control is \
             disabled"
        );
        return;
    }
    info!("Control socket listening on {name}");

    let handle = PipeHandle(pipe);
    if let Err(e) = std::thread::Builder::new()
        .name("control-socket".into())
        .spawn(move || serve(handle))
    {
        warn!("Failed to spawn the control-socket thread: {e}");
    }
}

/// Serve connections on a single pipe instance: connect, handle one command,
/// wait for a client to close, disconnect, repeat.
fn serve(handle: PipeHandle) {
    // One manual-reset event reused by every overlapped operation on this
    // instance (accept, read, write, close-wait); the serve loop runs one
    // operation at a time, so a single event suffices. The event lives as
    // long as the serve loop, which never exits.
    let event =
        match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
            Ok(event) => event,
            Err(_) => {
                warn!(
                    "Could not create the control-pipe wait event; runtime \
                     log-level control is disabled"
                );
                return;
            }
        };
    loop {
        if !accept_client(handle, event) {
            continue;
        }
        let mut conn = Pipe { handle, event };
        if let Err(e) = handle_connection(&mut conn) {
            debug!("control-socket connection ended: {e}");
        } else {
            // The response sits in the pipe buffer; the client reads it at
            // its own pace and closes afterwards.  Wait for that close
            // before disconnecting, because `DisconnectNamedPipe` resets
            // the instance and discards any unread data — disconnecting
            // first would make the client's read fail with
            // `ERROR_PIPE_NOT_CONNECTED`.
            wait_for_client_close(&mut conn);
        }
        unsafe {
            let _ = DisconnectNamedPipe(conn.handle.0);
        }
    }
}

/// Wait (bounded by [`IO_TIMEOUT`]) for a client to connect.
///
/// On an overlapped pipe instance `ConnectNamedPipe` returns immediately:
/// either the connection already completed (`ERROR_PIPE_CONNECTED`) or the
/// request was queued (`ERROR_IO_PENDING`) and its event signals once a
/// client connects. A peer that never shows up is abandoned after one
/// timeout; the instance stays listening, so the caller simply retries.
fn accept_client(handle: PipeHandle, event: HANDLE) -> bool {
    let mut ov = OVERLAPPED {
        hEvent: event,
        ..Default::default()
    };
    // A stale signal from a previous completion would make the wait return
    // before a client has connected.
    let _ = unsafe { ResetEvent(event) };
    match unsafe { ConnectNamedPipe(handle.0, Some(&raw mut ov)) } {
        Ok(()) => true,
        Err(_) => {
            let code = unsafe { GetLastError() };
            if code == ERROR_PIPE_CONNECTED {
                // A client connected before the call was issued.
                return true;
            }
            if code == ERROR_IO_PENDING
                && wait_overlapped(handle.0, &ov).is_ok()
            {
                return true;
            }
            debug!("control-pipe accept ended (error {:#010x})", code.0);
            // Reset the instance so the next accept attempt can connect.
            unsafe {
                let _ = DisconnectNamedPipe(handle.0);
            }
            false
        }
    }
}

/// Block until the client closes its end of the pipe.
///
/// After a successful exchange the client writes nothing more; the read
/// therefore stays pending until the peer goes away.  A peer close surfaces
/// as `ERROR_BROKEN_PIPE`, which [`Pipe::read`] maps to `Ok(0)` (EOF), so a
/// zero-length read is the close signal and ends the wait. Each read is
/// bounded by [`IO_TIMEOUT`], so a client that keeps its handle open
/// without closing is abandoned after one timeout instead of wedging the
/// single pipe instance.
fn wait_for_client_close(conn: &mut Pipe) {
    let mut buf = [0u8; 64];
    loop {
        match conn.read(&mut buf) {
            // The client closed; the response has been consumed.
            Ok(0) => break,
            // Ignore stray data; keep waiting for the close.
            Ok(_) => {}
            // Any error means the peer is gone (or the pipe is unusable);
            // either way there is nothing more to wait for.
            Err(_) => break,
        }
    }
}

/// Open the control pipe as a client.
pub(super) fn connect() -> std::io::Result<Box<dyn IoStream>> {
    connect_to(PIPE_NAME)
}

/// Open the pipe at *name* as a client.
///
/// The daemon serves one connection at a time on a single pipe instance and
/// releases it only after the previous client's end closed, so an open can
/// land in the tiny window between that close and the daemon's
/// `DisconnectNamedPipe` and fail with `ERROR_PIPE_BUSY`.  `WaitNamedPipeW`
/// blocks until the instance is free again, then the open is retried.
fn connect_to(name: &str) -> std::io::Result<Box<dyn IoStream>> {
    let name = to_wide(name);
    let mut last_busy = None;
    for _ in 0..BUSY_RETRY_ATTEMPTS {
        match open_pipe(&name) {
            Ok(handle) => {
                return Ok(Box::new(ClientPipe {
                    handle: PipeHandle(handle),
                }));
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                last_busy = Some(e);
                unsafe {
                    let _ = WaitNamedPipeW(
                        PCWSTR(name.as_ptr()),
                        BUSY_RETRY_WAIT_MS,
                    );
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_busy.expect("the loop exits only on a busy open"))
}

/// One attempt to open the pipe at *name*.
fn open_pipe(name: &[u16]) -> std::io::Result<HANDLE> {
    let handle = match unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_ALL.0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    } {
        Ok(handle) => handle,
        Err(_) => {
            let code = unsafe { GetLastError() };
            return Err(std::io::Error::from_raw_os_error(code.0 as i32));
        }
    };
    Ok(handle)
}

/// Convert a failed client connection into a friendly message for the CLI.
pub(super) fn connect_error(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(ERROR_FILE_NOT_FOUND) => "no daemon is running (or its control \
                                       pipe does not exist)"
            .to_string(),
        Some(ERROR_PIPE_BUSY) => {
            "the daemon control pipe is busy; try again".to_string()
        }
        _ => e.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        super::{read_frame, write_frame},
        *,
    };

    /// The hand-rolled ACL must be self-consistent: the header's `AclSize`
    /// must equal the header plus exactly one ACE, and the ACE's size field
    /// must equal its fixed part plus the SID.  Inconsistent sizes are
    /// rejected by the kernel on recent Windows builds.
    #[test]
    fn owner_only_descriptor_builds_a_consistent_acl() {
        // A 28-byte user SID (revision 1, NT authority, five subauthorities).
        let mut sid = vec![0u8; 28];
        sid[0] = 1;
        sid[1] = 5;
        let Some(desc) = owner_only_descriptor(&sid) else {
            panic!("the descriptor could not be built");
        };
        let acl = &desc.acl;
        assert_eq!(acl[0], ACL_REVISION.0 as u8, "wrong ACL revision");
        let acl_size = u16::from_le_bytes([acl[2], acl[3]]) as usize;
        let ace_count = u16::from_le_bytes([acl[4], acl[5]]);
        assert_eq!(
            acl_size,
            ACL_HEADER_BYTES + ACE_FIXED_BYTES + sid.len(),
            "AclSize does not cover exactly one ACE"
        );
        assert_eq!(acl_size, acl.len(), "AclSize must equal the buffer");
        assert_eq!(ace_count, 1, "expected exactly one ACE");
        assert_eq!(acl[8], 0, "expected an ACCESS_ALLOWED ACE");
        assert_eq!(acl[9], 0, "unexpected ACE flags");
        let ace_size = u16::from_le_bytes([acl[10], acl[11]]) as usize;
        assert_eq!(
            ace_size,
            ACE_FIXED_BYTES + sid.len(),
            "ACE size does not cover fixed part + SID"
        );
        assert_eq!(
            &acl[12..16],
            &GENERIC_ALL.0.to_le_bytes(),
            "wrong ACE mask"
        );
        assert_eq!(&acl[16..], &sid[..], "SID was not copied into the ACE");
        assert_eq!(
            desc.sd.Revision, SD_REVISION as u8,
            "wrong security descriptor revision"
        );
        assert_eq!(
            desc.sd.Dacl as *const () as usize,
            acl.as_ptr() as usize,
            "the DACL must point at the owned buffer"
        );
    }

    /// The production path end to end: create the pipe without a security
    /// descriptor, apply the owner-only DACL, and serve.  A client of the
    /// same user (like the CLI) must be able to connect and round-trip a
    /// command.  Before the two-step approach, `CreateNamedPipeW` with the
    /// descriptor failed on recent Windows builds and the pipe never came
    /// up.
    ///
    /// The serve thread is intentionally left running; it dies with the test
    /// process.
    #[test]
    fn start_with_name_serves_the_owner() {
        let name =
            format!("\\\\.\\pipe\\keymapperd_prod_{}", std::process::id());
        start_with_name(&name);
        let mut conn = connect_to(&name).expect("the owner could not connect");
        write_frame(&mut conn, "SET-LOG-LEVEL info")
            .expect("the write failed");
        let reply = read_frame(&mut conn).expect("the read failed");
        assert!(reply.starts_with("OK "), "unexpected reply: {reply}");
    }

    /// The client must close its handle on drop, so the daemon's single pipe
    /// instance is released for the next connection.  Before the fix the
    /// handle leaked until process exit: a long-lived client (the e2e
    /// harness) kept the instance connected after its probe exchange, and the
    /// phase's `SET-LOG-LEVEL` connect failed with `ERROR_PIPE_BUSY`.  With
    /// the leak back, the second connect's busy retries would all time out
    /// and the test would fail.
    ///
    /// The serve thread is intentionally left running; it dies with the test
    /// process.
    #[test]
    fn client_drop_frees_the_pipe_instance() {
        let name =
            format!("\\\\.\\pipe\\keymapperd_repro_{}", std::process::id());
        // No security descriptor: this test pins the close-on-drop behaviour
        // only; the full production path (creation plus the owner-only DACL)
        // is covered by `start_with_name_serves_the_owner`.
        let wide_name = to_wide(&name);
        let pipe = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide_name.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(
                    PIPE_ACCESS_DUPLEX.0
                        | FILE_FLAG_FIRST_PIPE_INSTANCE.0
                        | FILE_FLAG_OVERLAPPED.0,
                ),
                NAMED_PIPE_MODE(
                    PIPE_TYPE_BYTE.0
                        | PIPE_READMODE_BYTE.0
                        | PIPE_REJECT_REMOTE_CLIENTS.0,
                ),
                1,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                None,
            )
        };
        assert!(
            !pipe.is_invalid(),
            "CreateNamedPipeW failed: {:?}",
            std::io::Error::last_os_error()
        );
        let handle = PipeHandle(pipe);
        std::thread::Builder::new()
            .name("control-socket".into())
            .spawn(move || serve(handle))
            .expect("failed to spawn the serve thread");

        // First exchange (like the e2e probe): connect, round-trip a frame,
        // and drop the connection.
        let mut first = connect_to(&name).expect("the first connect failed");
        write_frame(&mut first, "SET-LOG-LEVEL info")
            .expect("the first write failed");
        let reply = read_frame(&mut first).expect("the first read failed");
        assert!(reply.starts_with("OK "), "unexpected reply: {reply}");
        drop(first);

        // Second exchange from the same process (like the e2e phase): this
        // only succeeds if dropping the first connection closed its handle
        // and the serve loop disconnected the instance.
        let mut second =
            connect_to(&name).expect("the pipe instance was not released");
        write_frame(&mut second, "SET-LOG-LEVEL info")
            .expect("the second write failed");
        let reply = read_frame(&mut second).expect("the second read failed");
        assert!(reply.starts_with("OK "), "unexpected reply: {reply}");
    }
}
