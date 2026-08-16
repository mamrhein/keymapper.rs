// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows-specific daemon spawning and termination via `CreateProcess` /
//! `TerminateProcess`.

use std::path::Path;

use windows::Win32::{
    Foundation::{BOOL, CloseHandle, FALSE, HANDLE, TRUE},
    System::Threading::{
        CREATE_NO_WINDOW, CreateProcessW, INFINITE, PROCESS_CREATION_FLAGS,
        PROCESS_CREATION_USE_INHERITED_HANDLES, PROCESS_INFORMATION,
        STARTUPINFOW, TerminateProcess, WaitForSingleObject,
    },
};

/// Check whether a process with the given PID is alive.
pub fn is_process_alive(pid: u32) -> bool {
    // Open the process with minimal access just to check if it exists.
    unsafe {
        match OpenProcessW(PROCESS_QUERY_INFORMATION, FALSE, pid) {
            Ok(handle) if !handle.is_invalid() => {
                let _ = CloseHandle(handle);
                true
            }
            _ => false,
        }
    }
}

/// Declares `OpenProcessW` which is needed to check if a process is alive.
#[allow(non_snake_case)]
unsafe extern "system" {
    fn OpenProcessW(
        dwDesiredAccess: u32,
        bInheritHandle: BOOL,
        dwProcessId: u32,
    ) -> HANDLE;
}

/// Access flag to query process information.  Defined in winnt.h but not
/// always exposed by the windows crate feature set.
#[allow(non_upper_case_globals)]
const PROCESS_QUERY_INFORMATION: u32 = 0x0400;

/// Spawn `keymapperd` as a background process without creating a console
/// window.  The working directory is set to the given config directory so
/// the daemon can find its config via CWD lookup.
pub fn spawn_daemon(
    config_dir: &Path,
) -> Result<(u32, Option<String>), String> {
    let exe_wide = to_wide("keymapperd.exe");
    let dir_wide = to_wide(&config_dir.to_string_lossy());

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // CREATE_NO_WINDOW suppresses the console window for console applications.
    let result = unsafe {
        CreateProcessW(
            None,                     /* lpApplicationName — parse from
                                       * command line */
            exe_wide.as_ptr().into(), // lpCommandLine
            None,                     // lpProcessAttributes
            None,                     // lpThreadAttributes
            PROCESS_CREATION_USE_INHERITED_HANDLES, /* bInheritHandles — do
                                                     * inherit */
            CREATE_NO_WINDOW,        // dwCreationFlags
            None,                    // lpEnvironment
            Some(dir_wide.as_ptr()), // lpCurrentDirectory
            &mut si,                 // lpStartupInfo
            &mut pi,                 // lpProcessInformation
        )
    };

    if result.is_ok() {
        // Close the handles returned by CreateProcessW.  The child is
        // independent and we don't need to track it.
        unsafe {
            let _ = CloseHandle(pi.hProcess);
            let _ = CloseHandle(pi.hThread);
        }
        Ok((pi.dwProcessId, None))
    } else {
        Err("failed to start keymapperd.exe".to_string())
    }
}

/// Terminate the daemon process.  Uses `TerminateProcess` which sends a hard
/// stop.  There is no equivalent to SIGTERM on Windows, so we open the process
/// and terminate it directly.
pub fn terminate_daemon(pid: u32) -> Result<(), String> {
    let handle = unsafe {
        // Request terminate access to kill the process.
        #[allow(non_upper_case_globals)]
        const PROCESS_TERMINATE: u32 = 0x0001;

        OpenProcessW(PROCESS_TERMINATE, FALSE, pid)
    };

    let Ok(handle) = handle else {
        return Err(format!("failed to open process {pid} for termination"));
    };

    if handle.is_invalid() {
        return Err(format!("process {pid} is not alive"));
    }

    // Attempt a graceful approach first: post a WM_CLOSE-like request is not
    // applicable for a background daemon, so we go straight to
    // TerminateProcess.
    let result = unsafe { TerminateProcess(handle, 0) };

    unsafe {
        let _ = CloseHandle(handle);
    }

    if result.as_bool() {
        Ok(())
    } else {
        Err(format!("failed to terminate process {pid}"))
    }
}

/// Convert a UTF-8 string to a null-terminated wide (UTF-16) string.
fn to_wide(s: &str) -> Vec<u16> {
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

    let os_str = OsStr::new(s);
    let encoded: Vec<u16> = os_str.encode_wide().collect();
    let mut wide = encoded;
    wide.push(0);
    wide
}
