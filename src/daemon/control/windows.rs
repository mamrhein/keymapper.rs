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
//! The daemon creates `\\.\pipe\keymapperd-<nonce>` and serves one command
//! per connection. The nonce is random per daemon run, so the full name is
//! unpredictable: a same-user process that starts before the daemon (e.g.
//! during the logon race against the scheduled task) cannot squat a fixed
//! name to steal the endpoint. The pipe is the auth mechanism: its DACL
//! grants only the current user (a single `ACCESS_ALLOWED` ACE for the
//! caller's SID) full control, so only that user can open it. This
//! supersedes the `daemon_token` removed in Phase 2.
//!
//! Discovery goes the other way around: once the pipe is live with its
//! owner-only DACL applied, the daemon publishes its exact name to
//! `%LOCALAPPDATA%\keymapperd\control.pipe`, hardened with the same
//! owner-only DACL. Because publication happens only after the pipe exists,
//! a readable publish file always names a live, genuine endpoint, and the
//! CLI never connects to a name it guessed itself. A missing or stale file
//! surfaces on the CLI as a clean "no daemon is running" error.
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
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
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
            ACL_REVISION, Cryptography::ProcessPrng, GetLengthSid,
            GetTokenInformation, InitializeSecurityDescriptor,
            PSECURITY_DESCRIPTOR, PSID, SECURITY_DESCRIPTOR,
            SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
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

/// The control pipe name prefix. The daemon appends a per-run random nonce
/// (see [`pipe_name`]), so the full name is unpredictable: a same-user
/// process that starts before the daemon cannot squat it.
const PIPE_PREFIX: &str = r"\\.\pipe\keymapperd-";

/// The root every published pipe name must live under. The CLI validates
/// the publish file against it, so a foreign file can never redirect the
/// client to an arbitrary object.
const PIPE_ROOT: &str = r"\\.\pipe\";

/// Random bytes in the per-run pipe-name nonce (rendered as 32 hex
/// characters).
const NONCE_BYTES: usize = 16;

/// The daemon's directory under `%LOCALAPPDATA%` (shared with the log
/// directory).
const APP_DIR_NAME: &str = "keymapperd";

/// The file that publishes the daemon's live pipe name to the CLI.
const PUBLISH_FILE_NAME: &str = "control.pipe";

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

/// The `ERROR_ACCESS_DENIED` code: with `FILE_FLAG_FIRST_PIPE_INSTANCE`,
/// another process already owns the first instance of the name.
const ERROR_ACCESS_DENIED: i32 = 5;

/// How long to keep retrying pipe opens and creations: on the client side
/// when the daemon's single instance is serving another client
/// (`ERROR_PIPE_BUSY`), and on the daemon side when a name is transiently
/// unavailable (see the creation retry in [`start_with_name`]). With a
/// per-run nonce the daemon's own name is effectively never contended, so
/// the daemon-side retries are purely defensive; 2 s is a comfortable upper
/// bound.
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

/// The control pipe name: the fixed prefix plus a per-run random nonce.
///
/// `None` only when the system PRNG fails; the caller must then disable the
/// endpoint rather than fall back to a predictable name.
fn pipe_name() -> Option<String> {
    let mut nonce = [0u8; NONCE_BYTES];
    let ok = unsafe {
        // SAFETY: `ProcessPrng` fills the caller's buffer in place; it
        // needs no algorithm handle or initialisation.
        ProcessPrng(&mut nonce)
    };
    if !ok.as_bool() {
        return None;
    }
    let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("{PIPE_PREFIX}{hex}"))
}

/// The file through which the daemon publishes its pipe name to the CLI:
/// `%LOCALAPPDATA%\keymapperd\control.pipe`. Both sides resolve it the
/// same way from the environment, so they always agree.
fn publish_path() -> Option<PathBuf> {
    Some(
        dirs::data_local_dir()?
            .join(APP_DIR_NAME)
            .join(PUBLISH_FILE_NAME),
    )
}

/// Publish *name* (the pipe the daemon is serving) at *path*.
///
/// The file is hardened with the same owner-only DACL as the pipe *before*
/// any content is written, and removed again when the hardening fails: a
/// file that cannot be locked down to the owner must not leak the name
/// (fail closed). Publication runs only after the pipe is live with its
/// DACL applied, so a readable publish file always names a genuine
/// endpoint.
fn publish_name(path: &Path, name: &str) -> Result<(), String> {
    let Some(sid) = current_user_sid() else {
        return Err("could not resolve the current user's SID".to_string());
    };
    let Some(desc) = owner_only_descriptor(&sid) else {
        return Err(
            "could not build an owner-only security descriptor".to_string()
        );
    };
    if let Some(parent) = path.parent() {
        fs_err::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let mut file = fs_err::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    // Harden before writing: the file only ever holds the name while it
    // is owner-only.
    let dacl_error = set_dacl(HANDLE(file.as_raw_handle()), &desc.sd);
    drop(desc);
    if dacl_error != 0 {
        drop(file);
        let _ = fs_err::remove_file(path);
        return Err(format!(
            "cannot apply the owner-only DACL to {} (error \
             {dacl_error:#010x})",
            path.display()
        ));
    }
    file.write_all(name.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Read the pipe name published at *path*.
///
/// `None` when the file is missing, unreadable, or does not name a pipe: a
/// stale or foreign file must never redirect the CLI to an arbitrary
/// object, and a missing name surfaces (via [`connect`]) as "no daemon".
fn resolve_published_name(path: &Path) -> Option<String> {
    let name = fs_err::read_to_string(path).ok()?;
    let name = name.trim();
    (!name.is_empty() && name.starts_with(PIPE_ROOT)).then(|| name.to_string())
}

/// Create the pipe and spawn the serve thread.
///
/// The pipe name carries a per-run random nonce (see [`pipe_name`]); once
/// the endpoint is live, its exact name is published to [`publish_path`]
/// so the CLI can discover it.
pub fn start() {
    let Some(name) = pipe_name() else {
        warn!(
            "Could not generate a control pipe nonce; runtime log-level \
             control is disabled"
        );
        return;
    };
    if !start_with_name(&name) {
        return;
    }
    let published = match publish_path() {
        Some(path) => publish_name(&path, &name),
        None => Err(format!(
            "no local data directory (LOCALAPPDATA) for {}",
            PUBLISH_FILE_NAME
        )),
    };
    if let Err(e) = published {
        warn!(
            "The control endpoint is live but its name could not be \
             published: {e}; the CLI will not find the running daemon"
        );
    }
}

/// Create the pipe at *name*, apply the owner-only DACL, and spawn the
/// serve thread. Returns whether the control endpoint is live.
fn start_with_name(name: &str) -> bool {
    let Some(sid) = current_user_sid() else {
        warn!(
            "Could not resolve the current user's SID; runtime log-level \
             control is disabled"
        );
        return false;
    };
    let Some(desc) = owner_only_descriptor(&sid) else {
        warn!(
            "Could not build an owner-only pipe security descriptor; runtime \
             log-level control is disabled"
        );
        return false;
    };
    // The ACE copied the SID bytes into the ACL, so the owned buffer only
    // needs to drop when this function returns (never `FreeSid`; see
    // `current_user_sid`). `desc` stays alive across the `SetSecurityInfo`
    // call below, which copies the DACL into the pipe object.

    let wide_name = to_wide(name);
    // The pipe is created without a security descriptor; the owner-only DACL
    // is applied right afterwards. Passing the descriptor to
    // `CreateNamedPipeW` directly is rejected on recent Windows builds.
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
            // Retry while the name is transiently unavailable: an earlier
            // instance is still connected (`ERROR_PIPE_BUSY`) or another
            // process holds the first instance (`ERROR_ACCESS_DENIED`).
            // With a per-run nonce both are practically impossible for the
            // daemon's own name, so the retry is purely defensive; the
            // bound keeps a persistent squatter from stalling startup.
            if !pipe.is_invalid()
                || (code.0 != ERROR_PIPE_BUSY as u32
                    && code.0 != ERROR_ACCESS_DENIED as u32)
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
        return false;
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
        return false;
    }
    info!("Control socket listening on {name}");

    let handle = PipeHandle(pipe);
    if let Err(e) = std::thread::Builder::new()
        .name("control-socket".into())
        .spawn(move || serve(handle))
    {
        warn!("Failed to spawn the control-socket thread: {e}");
        return false;
    }
    true
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
///
/// The daemon's pipe name contains a per-run nonce, so the CLI does not
/// guess a fixed name; it resolves the name from the daemon's publish file
/// (see [`resolve_published_name`]). A missing or unparsable file is
/// reported as [`std::io::ErrorKind::NotFound`], which [`connect_error`]
/// renders like a missing daemon.
pub(super) fn connect() -> std::io::Result<Box<dyn IoStream>> {
    let path = publish_path().ok_or_else(no_endpoint_error)?;
    let name = resolve_published_name(&path).ok_or_else(no_endpoint_error)?;
    connect_to(&name)
}

/// The error for "no published control endpoint": no local data directory
/// to look in, or no readable publish file in it.
fn no_endpoint_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "the running daemon published no control endpoint",
    )
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
        Some(ERROR_FILE_NOT_FOUND) => "no daemon is running (or it published \
                                       no control endpoint)"
            .to_string(),
        Some(ERROR_PIPE_BUSY) => {
            "the daemon control pipe is busy; try again".to_string()
        }
        // Covers `no_endpoint_error` (no publish file) and any other
        // "not found" that means no reachable daemon.
        _ if e.kind() == std::io::ErrorKind::NotFound => {
            "no daemon is running (it published no control endpoint)"
                .to_string()
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
        assert!(start_with_name(&name));
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

    /// Pipe names are unpredictable: the fixed prefix plus a fresh 32-
    /// character hex nonce on every draw. This is what removes the
    /// predictable-name logon race.
    #[test]
    fn pipe_names_are_unique_and_prefixed() {
        let a = pipe_name().expect("the system PRNG failed");
        let b = pipe_name().expect("the system PRNG failed");
        assert!(a.starts_with(PIPE_PREFIX));
        assert!(b.starts_with(PIPE_PREFIX));
        assert_ne!(a, b, "consecutive nonces must differ");
        let nonce = a.strip_prefix(PIPE_PREFIX).unwrap();
        assert_eq!(nonce.len(), 2 * NONCE_BYTES);
        assert!(nonce.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The discovery channel end to end (with a temp publish file instead
    /// of the real `%LOCALAPPDATA%` one): publish the live pipe's name,
    /// resolve it back, and round-trip a command through the resolved
    /// name.
    ///
    /// The serve thread is intentionally left running; it dies with the
    /// test process.
    #[test]
    fn published_endpoint_round_trips_through_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PUBLISH_FILE_NAME);
        let name =
            format!("\\\\.\\pipe\\keymapperd_disc_{}", std::process::id());
        assert!(start_with_name(&name));
        publish_name(&path, &name).expect("publishing failed");
        let resolved =
            resolve_published_name(&path).expect("discovery failed");
        assert_eq!(resolved, name);
        let mut conn =
            connect_to(&resolved).expect("the owner could not connect");
        write_frame(&mut conn, "SET-LOG-LEVEL info")
            .expect("the write failed");
        let reply = read_frame(&mut conn).expect("the read failed");
        assert!(reply.starts_with("OK "), "unexpected reply: {reply}");
    }

    /// A missing, empty, or foreign publish file must never yield a name:
    /// the CLI then reports "no daemon" instead of connecting to an
    /// arbitrary object.
    #[test]
    fn resolve_rejects_missing_or_foreign_publish_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PUBLISH_FILE_NAME);
        assert_eq!(resolve_published_name(&path), None);
        fs_err::write(&path, "not a pipe name\n").unwrap();
        assert_eq!(resolve_published_name(&path), None);
        fs_err::write(&path, "   \n").unwrap();
        assert_eq!(resolve_published_name(&path), None);
    }

    /// A stale publish file from a previous daemon run names a pipe that
    /// nothing serves any more: the connect fails and the CLI message
    /// stays "no daemon is running".
    #[test]
    fn stale_publish_file_reports_no_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PUBLISH_FILE_NAME);
        let stale =
            format!("{}keymapperd-{}", PIPE_ROOT, "0".repeat(2 * NONCE_BYTES));
        fs_err::write(&path, &stale).unwrap();
        let name =
            resolve_published_name(&path).expect("the name must resolve");
        let err = match connect_to(&name) {
            Ok(_) => panic!("a dead name must not connect"),
            Err(e) => e,
        };
        let msg = connect_error(&err);
        assert!(
            msg.contains("no daemon is running"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn connect_error_classifies() {
        let e = std::io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND);
        assert!(connect_error(&e).contains("no daemon is running"));
        let e = std::io::Error::from_raw_os_error(ERROR_PIPE_BUSY);
        assert!(connect_error(&e).contains("busy"));
        assert!(
            connect_error(&no_endpoint_error())
                .contains("no daemon is running")
        );
    }
}
