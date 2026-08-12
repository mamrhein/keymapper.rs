// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Synchronous foreground application query via Win32 APIs.

use windows::Win32::{
    Foundation::CloseHandle,
    System::Threading::{
        OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
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
    if hwnd.is_invalid() {
        return "unknown".to_string();
    }

    // Get the process ID of the thread that owns the foreground window.
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return "unknown".to_string();
    }

    // Open the process with minimal permissions to query its image name.
    let Ok(process) = (unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
    }) else {
        return "unknown".to_string();
    };
    if process.is_invalid() {
        return "unknown".to_string();
    }

    // Query the full process image path (wide string).
    let mut buffer = [0u16; 512]; // MAX_PATH * 2, sufficient for executable paths.
    let mut size = buffer.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };

    if ok.is_err() {
        // CloseHandle fails only with an invalid handle, which would be a bug.
        let _ = unsafe { CloseHandle(process) };
        return "unknown".to_string();
    }

    // CloseHandle fails only with an invalid handle, which would be a bug.
    let _ = unsafe { CloseHandle(process) };

    // Extract the executable name from the full path.  The path uses backslash
    // separators (e.g. "C:\Windows\System32\notepad.exe"), so we find the last
    // backslash and take the file name from there.
    if let Ok(path) = String::from_utf16(&buffer[..size as usize])
        && let Some(stem) = path.rsplit('\\').next()
    {
        return stem.to_string();
    }

    "unknown".to_string()
}
