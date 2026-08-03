// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Synchronous foreground application query via Win32 APIs.

use windows_sys::Win32::{
    Foundation::{CloseHandle, FALSE, HWND},
    System::ProcessStatus::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    },
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

/// Synchronously query the current foreground application name.
///
/// Resolves the foreground window to its owning process and extracts the
/// executable name from the full image path.  Returns `"unknown"` when the
/// query fails or no window is in the foreground.
pub fn get_active_app_name() -> String {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd == HWND(0) {
        return "unknown".to_string();
    }

    // Get the process ID of the thread that owns the foreground window.
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return "unknown".to_string();
    }

    // Open the process with minimal permissions to query its image name.
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if process.is_empty() {
        return "unknown".to_string();
    }

    // Query the full process image path (wide string).
    let mut buffer = [0u16; 512]; // MAX_PATH * 2, sufficient for executable paths.
    let mut size = buffer.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size)
    };

    if ok == 0 {
        unsafe { CloseHandle(process) };
        return "unknown".to_string();
    }

    unsafe { CloseHandle(process) };

    // Extract the executable name from the full path.  The path uses backslash
    // separators (e.g. "C:\Windows\System32\notepad.exe"), so we find the last
    // backslash and take the file name from there.
    if let Some(path) = String::from_utf16(&buffer[..size as usize]).ok() {
        if let Some(stem) = path.rsplit('\\').next() {
            return stem.to_string();
        }
    }

    "unknown".to_string()
}
