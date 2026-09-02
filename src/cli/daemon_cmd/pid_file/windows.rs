// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows-specific daemon spawning and termination via `CreateProcessW` /
//! `TerminateProcess`.

use std::path::Path;

use windows::Win32::{
    Foundation::CloseHandle,
    System::Threading::{
        CREATE_NO_WINDOW, CreateProcessW, OpenProcess, PROCESS_INFORMATION,
        PROCESS_NAME_FORMAT, PROCESS_QUERY_INFORMATION, PROCESS_TERMINATE,
        QueryFullProcessImageNameW, STARTUPINFOW, TerminateProcess,
    },
};

/// Check whether a process with the given PID is alive.
pub fn is_process_alive(pid: u32) -> bool {
    // Open the process with minimal access just to check if it exists. The
    // windows crate already reports an error for invalid handles, so a
    // successful open is the liveness probe.
    let probe = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, false, pid) };

    match probe {
        Ok(handle) => {
            // The handle is only needed for the probe, so close it right away.
            unsafe {
                let _ = CloseHandle(handle);
            }
            true
        }
        Err(_) => false,
    }
}

/// The daemon binary name, as it appears in the process image path.
const DAEMON_NAME: &str = "keymapperd.exe";

/// Verify that the process with the given PID is actually `keymapperd.exe` by
/// querying its full image path via `QueryFullProcessImageNameW`.  Returns
/// `false` when the process cannot be opened or its image is not named
/// `keymapperd.exe` (e.g. an unrelated process that reused the PID).
pub fn verify_daemon_identity(pid: u32) -> bool {
    let handle =
        match unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, false, pid) } {
            Ok(handle) => handle,
            Err(_) => return false,
        };

    let mut buf = [0u16; 1024];
    let mut size = buf.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR::from_raw(buf.as_mut_ptr()),
            &mut size,
        )
    };

    // Always release the process handle, even if the query failed.
    unsafe {
        let _ = CloseHandle(handle);
    }

    match result {
        // On success `size` holds the number of characters written, excluding
        // the terminating NUL.
        Ok(()) => {
            let path = String::from_utf16_lossy(&buf[..size as usize]);
            std::path::Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == DAEMON_NAME)
        }
        Err(_) => false,
    }
}

/// Spawn `keymapperd` as a background process without creating a console
/// window.  The working directory is set to the given config directory so
/// the daemon can find its config via CWD lookup.
pub fn spawn_daemon(
    config_dir: &Path,
) -> Result<(u32, Option<String>), String> {
    let mut exe_wide = to_wide("keymapperd.exe");
    let dir_wide = to_wide(&config_dir.to_string_lossy());

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // Build the wide string arguments for CreateProcessW.
    let command_line = windows::core::PWSTR::from_raw(exe_wide.as_mut_ptr());
    let current_dir = windows::core::PCWSTR::from_raw(dir_wide.as_ptr());

    // CREATE_NO_WINDOW suppresses the console window for console applications.
    let result = unsafe {
        CreateProcessW(
            None,               /* lpApplicationName — parse from the
                                 * command line. */
            Some(command_line), // lpCommandLine
            None,               // lpProcessAttributes
            None,               // lpThreadAttributes
            true,               /* bInheritHandles — let the daemon inherit
                                 * our */
            // handles.
            CREATE_NO_WINDOW, // dwCreationFlags
            None,             // lpEnvironment
            current_dir,      // lpCurrentDirectory
            &si,              // lpStartupInfo
            &mut pi,          // lpProcessInformation
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
    let handle = match unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) } {
        Ok(handle) => handle,
        Err(_) => {
            return Err(format!(
                "failed to open process {pid} for termination"
            ));
        }
    };

    let result = unsafe { TerminateProcess(handle, 0) };

    // Always release the process handle, even if the termination failed.
    unsafe {
        let _ = CloseHandle(handle);
    }

    if result.is_ok() {
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
