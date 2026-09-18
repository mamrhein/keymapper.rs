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
//! The daemon creates `\\.\pipe\keymapperd` and serves one command per
//! connection. The pipe is the auth mechanism: its security descriptor grants
//! only the current user (a single `ACCESS_ALLOWED` ACE for the caller's SID)
//! full control, so only that user can open it. This supersedes the
//! `daemon_token` removed in Phase 2.
//!
//! The owner-only security descriptor is built on the stack (an `ACL` with one
//! `ACCESS_ALLOWED_ACE` plus the SID) and freed right after the pipe is
//! created, because `CreateNamedPipeW` copies it into the pipe object.

use std::io::{Read, Write};

use log::{debug, info, warn};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_BROKEN_PIPE, FALSE, GENERIC_ALL, GetLastError,
            HANDLE,
        },
        Security::{
            ACL, ACL_REVISION, AddAccessAllowedAce, FreeSid, GetLengthSid,
            GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor,
            PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
            SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl, TOKEN_USER,
            TokenUser,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE,
            FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING,
            PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
        },
        System::{
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe,
                NAMED_PIPE_MODE, PIPE_READMODE_BYTE,
                PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
            },
            Threading::{GetCurrentProcess, OpenProcessToken, TOKEN_QUERY},
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

/// Real size of the Windows `ACL` header (the Rust struct omits the trailing
/// `sizeOfFirstAce` field that `InitializeAcl` fills in).
const ACL_HEADER_BYTES: usize = 10;

/// Real size of one `ACCESS_ALLOWED_ACE`.
const ACE_BYTES: usize = 16;

/// The `ERROR_FILE_NOT_FOUND` code: no pipe to open means no daemon.
const ERROR_FILE_NOT_FOUND: i32 = 2;

/// The `ERROR_PIPE_BUSY` code: the daemon exists but is serving a client.
const ERROR_PIPE_BUSY: i32 = 231;

/// A named-pipe handle as a `Read`/`Write` stream.
///
/// `ReadFile`/`WriteFile` on a synchronous byte-mode pipe block until data is
/// available, so a peer close surfaces as `ERROR_BROKEN_PIPE` (mapped to EOF)
/// or an I/O error.
struct Pipe {
    handle: HANDLE,
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut bytes_read = 0u32;
        match unsafe {
            ReadFile(
                self.handle,
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

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut bytes_written = 0u32;
        match unsafe {
            WriteFile(
                self.handle,
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

/// An owner-only security descriptor, keeping the backing `ACL` buffer and the
/// `SECURITY_DESCRIPTOR` alive together. The `SECURITY_ATTRIBUTES` points at
/// the embedded `SECURITY_DESCRIPTOR`, whose DACL points at `acl`.
struct OwnerOnlyDescriptor {
    attrs: SECURITY_ATTRIBUTES,
    sd: SECURITY_DESCRIPTOR,
    acl: Vec<u8>,
}

/// The caller's primary SID, from the current process token.
fn current_user_sid() -> Option<PSID> {
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

        // Copy the `TOKEN_USER` out of the scratch buffer; the `Sid` it holds
        // is owned by the system and must be freed with `FreeSid` (by the
        // caller, once the ACE has copied its bytes into the ACL).
        let token_user: TOKEN_USER =
            core::ptr::read_unaligned(buffer.as_ptr() as *const TOKEN_USER);
        let sid = token_user.User.Sid;
        if sid.0.is_null() { None } else { Some(sid) }
    }
}

/// Build an owner-only descriptor granting *sid* full control of the pipe.
///
/// Returns `None` when the ACL or descriptor cannot be built (in which case
/// the pipe is not created, so it is never exposed with weaker-than-owner-only
/// security).
fn owner_only_descriptor(sid: PSID) -> Option<OwnerOnlyDescriptor> {
    let sid_len = unsafe { GetLengthSid(sid) } as usize;
    // Real layout: 10-byte ACL header + one 16-byte ACE + the SID bytes.
    let acl_size = ACL_HEADER_BYTES + ACE_BYTES + sid_len;
    let mut acl = vec![0u8; acl_size];
    let acl_ptr = acl.as_mut_ptr() as *mut ACL;

    if unsafe { InitializeAcl(acl_ptr, acl_size as u32, ACL_REVISION) }
        .is_err()
    {
        return None;
    }
    if unsafe {
        AddAccessAllowedAce(acl_ptr, ACL_REVISION, GENERIC_ALL.0, sid)
    }
    .is_err()
    {
        return None;
    }

    let mut sd: SECURITY_DESCRIPTOR = core::mem::zeroed();
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

    // Build the struct, then point `attrs` at its own `sd` (which cannot be
    // done while the struct is still being constructed).
    let mut desc = OwnerOnlyDescriptor {
        attrs: SECURITY_ATTRIBUTES {
            nLength: core::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            bInheritHandle: FALSE,
            lpSecurityDescriptor: core::ptr::null_mut(),
        },
        sd,
        acl,
    };
    desc.attrs.lpSecurityDescriptor =
        &desc.sd as *const _ as *mut core::ffi::c_void;
    Some(desc)
}

/// The NUL-terminated UTF-16 form of *name*, for the `W` Win32 APIs.
fn to_wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Create the pipe and spawn the serve thread.
pub fn start() {
    let Some(sid) = current_user_sid() else {
        warn!(
            "could not resolve the current user's SID; runtime log-level \
             control is disabled"
        );
        return;
    };
    let Some(desc) = owner_only_descriptor(sid) else {
        warn!(
            "could not build an owner-only pipe security descriptor; runtime \
             log-level control is disabled"
        );
        unsafe {
            FreeSid(sid);
        }
        return;
    };
    // The ACE copied the SID bytes into the ACL, so the token's SID can be
    // freed now; `desc` (and its ACL) stay alive across the `CreateNamedPipeW`
    // call, which copies the descriptor.
    unsafe {
        FreeSid(sid);
    }

    let name = to_wide(PIPE_NAME);
    let pipe = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(
                PIPE_ACCESS_DUPLEX.0 | FILE_FLAG_FIRST_PIPE_INSTANCE.0,
            ),
            NAMED_PIPE_MODE(
                PIPE_TYPE_BYTE.0
                    | PIPE_READMODE_BYTE.0
                    | PIPE_REJECT_REMOTE_CLIENTS.0,
            ),
            1, // one instance: one client at a time, matching the unix path
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            Some(&desc.attrs as *const _),
        )
    };
    drop(desc);

    if pipe.is_invalid() {
        let code = unsafe { GetLastError() };
        warn!(
            "could not create the control pipe {PIPE_NAME} (error {:#010x}); \
             runtime log-level control is disabled",
            code.0
        );
        return;
    }
    info!("control socket listening on {PIPE_NAME}");

    if let Err(e) = std::thread::Builder::new()
        .name("control-socket".into())
        .spawn(move || serve(pipe))
    {
        warn!("failed to spawn the control-socket thread: {e}");
    }
}

/// Serve connections on a single pipe instance: connect, handle one command,
/// disconnect, repeat.
fn serve(handle: HANDLE) {
    loop {
        // With no default timeout, `ConnectNamedPipe` blocks until a client
        // connects; if one is already waiting it returns
        // `ERROR_PIPE_CONNECTED`, which we treat as a success.
        unsafe {
            let _ = ConnectNamedPipe(handle, None);
        }
        let mut conn = Pipe { handle };
        if let Err(e) = handle_connection(&mut conn) {
            debug!("control-socket connection ended: {e}");
        }
        unsafe {
            let _ = DisconnectNamedPipe(handle);
        }
    }
}

/// Open the control pipe as a client.
pub(super) fn connect() -> std::io::Result<Box<dyn IoStream>> {
    let name = to_wide(PIPE_NAME);
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
    Ok(Box::new(Pipe { handle }))
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
