// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW,
        Process32NextW, TH32CS_SNAPPROCESS,
    },
};

/// Creation flag to suppress console window creation.
#[allow(non_upper_case_globals)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// STARTUPINFOW — describes how a new process should start.
#[allow(clippy::upper_case_acronyms)]
#[allow(non_snake_case)]
#[repr(C)]
struct STARTUPINFOW {
    cb: u32,
    lpReserved: *mut u16,
    lpDesktop: *mut u16,
    lpTitle: *mut u16,
    dwX: u32,
    dwY: u32,
    dwXSize: u32,
    dwYSize: u32,
    dwXCountChars: u32,
    dwYCountChars: u32,
    dwFillAttribute: u32,
    dwFlags: u32,
    wShowWindow: u16,
    cbReserved2: u16,
    lpReserved2: *mut u8,
    hStdInput: HANDLE,
    hStdOutput: HANDLE,
    hStdError: HANDLE,
}

/// PROCESS_INFORMATION — receives information about a new process.
#[allow(non_snake_case)]
#[repr(C)]
struct PROCESS_INFORMATION {
    hProcess: HANDLE,
    hThread: HANDLE,
    dwProcessId: u32,
    dwThreadId: u32,
}

// Declare `CreateProcessW` directly, since it is not exposed by our feature set.
#[allow(non_snake_case)]
unsafe extern "system" {
    fn CreateProcessW(
        lpApplicationName: *mut u16,
        lpCommandLine: *mut u16,
        lpProcessAttributes: *mut std::ffi::c_void,
        lpThreadAttributes: *mut std::ffi::c_void,
        bInheritHandles: i32,
        dwCreationFlags: u32,
        lpEnvironment: *mut std::ffi::c_void,
        lpCurrentDirectory: *mut u16,
        lpStartupInfo: *mut STARTUPINFOW,
        lpProcessInformation: *mut PROCESS_INFORMATION,
    ) -> i32;
}

/// Check whether a process with the given name is running by enumerating
/// processes via the ToolHelp32 API.  Uses native Windows APIs instead of
/// spawning `tasklist`, avoiding shell injection and string-matching
/// fragility.
pub fn is_daemon_running(name: &str) -> bool {
    // Normalise the image name — always compare against the `.exe` form.
    let target_name = if name.ends_with(".exe") {
        name.to_string()
    } else {
        format!("{name}.exe")
    };
    let target = to_wide(&target_name);

    let snapshot: HANDLE =
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return false;
    }

    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

    let mut found = false;

    // Process32First returns 1 on success, 0 on failure.
    if unsafe { Process32FirstW(snapshot, &mut entry) } == 1 {
        loop {
            if wide_eq(&entry.szExeFile, &target) {
                found = true;
                break;
            }
            if unsafe { Process32NextW(snapshot, &mut entry) } != 1 {
                break;
            }
        }
    }

    unsafe { CloseHandle(snapshot) };
    found
}

/// Convert a UTF-8 string to a null-terminated wide (UTF-16) string suitable
/// for Windows API calls.
fn to_wide(s: &str) -> Vec<u16> {
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

    let os_str = OsStr::new(s);
    let encoded: Vec<u16> = os_str.encode_wide().collect();
    // Append null terminator.
    let mut wide = encoded;
    wide.push(0);
    wide
}

/// Compare two null-terminated wide strings for equality, stopping at the
/// first null (U+0000) code unit in either string.
fn wide_eq(a: &[u16], b: &[u16]) -> bool {
    let a_len = a.iter().position(|&c| c == 0).unwrap_or(a.len());
    let b_len = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    if a_len != b_len {
        return false;
    }
    a[..a_len] == b[..b_len]
}

/// Spawn the daemon as a background process without creating a console window.
///
/// Uses `CreateProcessW` directly instead of going through `cmd.exe`, avoiding
/// any command injection surface from shell interpretation.
pub fn spawn_daemon(name: &str) -> Result<(), String> {
    // Pin the wide command string so the reference outlives the unsafe block.
    let cmd_wide = to_wide(name);
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // CREATE_NO_WINDOW ensures no console window is created for console
    // applications.  The lpApplicationName parameter is null so the full
    // executable name (including path lookup) is parsed from lpCommandLine.
    let result = unsafe {
        CreateProcessW(
            std::ptr::null_mut(), // lpApplicationName: parse from command line
            cmd_wide.as_ptr() as *mut u16, // lpCommandLine: mutable per API contract
            std::ptr::null_mut(), // lpProcessAttributes
            std::ptr::null_mut(), // lpThreadAttributes
            0, // bInheritHandles
            CREATE_NO_WINDOW, // dwCreationFlags: no console window
            std::ptr::null_mut(), // lpEnvironment
            std::ptr::null_mut(), // lpCurrentDirectory
            &si as *const _ as *mut STARTUPINFOW, // lpStartupInfo: mutable per API contract
            &mut pi,
        )
    };

    if result != 0 {
        // Close the handles returned by CreateProcessW. The child process
        // is independent; we don't need to track it.
        let proc_handle: HANDLE = pi.hProcess;
        let thread_handle: HANDLE = pi.hThread;
        unsafe {
            CloseHandle(proc_handle);
            CloseHandle(thread_handle);
        }
        Ok(())
    } else {
        Err(format!("failed to start {name}"))
    }
}
