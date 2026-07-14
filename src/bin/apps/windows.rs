// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Lists visible application names on Windows.
//!
//! The returned `app_name` is the `FileDescription` from the PE version
//! resources of the process executable, falling back to the file stem.
//! This matches exactly what `active-win-pos-rs` returns on Windows.

use std::{collections::HashSet, path::Path};

use windows_sys::Win32::{
    Foundation::{FALSE, HWND, TRUE},
    System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW,
        TH32CS_SNAPMODULE,
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetDesktopWindow, GetWindowThreadProcessId,
        IsWindowVisible,
    },
};

type SnapshotHandle = isize;

/// Convert a null-terminated UTF-16 slice to a Rust String.
fn utf16_to_string(data: &[u16]) -> String {
    let end = data.iter().position(|&c| c == 0).unwrap_or(data.len());
    String::from_utf16_lossy(&data[..end])
}

/// Convert a string to a null-terminated UTF-16 vector.
fn to_utf16_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Look up a value from the PE version resource block.
///
/// Returns `None` if the file has no version info or the key is missing.
unsafe fn ver_query_value(buffer: &[u8], sub_block: &str) -> Option<Vec<u16>> {
    let sub_block_utf16 = to_utf16_null(sub_block);
    let mut lplp_buffer: *const u8 = std::ptr::null();
    let mut pu_len: u32 = 0;

    if windows_sys::Win32::Storage::FileSystem::VerQueryValueW(
        buffer.as_ptr() as _,
        sub_block_utf16.as_ptr() as _,
        &mut lplp_buffer as *const _ as *mut _,
        &mut pu_len,
    ) == FALSE.as_i32()
    {
        return None;
    }

    if pu_len == 0 || lplp_buffer.is_null() {
        return None;
    }

    Some(
        std::slice::from_raw_parts(lplp_buffer as *const u16, pu_len as usize)
            .to_vec(),
    )
}

/// Resolve the actual language-specific `FileDescription` sub-block path by
/// reading the translation table from the version resource.
unsafe fn resolve_file_description_path(buffer: &[u8]) -> Option<String> {
    let lang_data = ver_query_value(buffer, "\\VarFileInfo\\Translation")?;
    if lang_data.len() < 2 {
        return None;
    }

    Some(format!(
        "\\StringFileInfo\\{:04x}{:04x}\\FileDescription",
        lang_data[0], lang_data[1]
    ))
}

/// Try to read the `FileDescription` from a PE file's version resources.
fn get_file_description(path: &str) -> Option<String> {
    let path_utf16 = to_utf16_null(path);

    unsafe {
        let size =
            windows_sys::Win32::Storage::FileSystem::GetFileVersionInfoSizeW(
                path_utf16.as_ptr(),
                std::ptr::null_mut(),
            );
        if size == 0 {
            return None;
        }

        let mut buffer = vec![0u8; size as usize];
        if windows_sys::Win32::Storage::FileSystem::GetFileVersionInfoW(
            path_utf16.as_ptr(),
            0,
            size,
            buffer.as_mut_ptr() as _,
        ) == FALSE.as_i32()
        {
            return None;
        }

        // Try the common English-US locale first, then fall back to the
        // actual translation block.
        if let Some(desc) = ver_query_value(
            &buffer,
            "\\StringFileInfo\\040904b0\\FileDescription",
        ) {
            let s = utf16_to_string(&desc);
            if !s.is_empty() {
                return Some(s);
            }
        }

        // Resolve from the actual translation table.
        if let Some(sub_block) = resolve_file_description_path(&buffer) {
            if let Some(desc) = ver_query_value(&buffer, &sub_block) {
                let s = utf16_to_string(&desc);
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }

        None
    }
}

/// Extract the file stem from a path (e.g., "chrome" from
/// "C:\Program Files\Google\Chrome\Application\chrome.exe").
fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// Get the executable path for a process by enumerating its modules.
fn get_process_exe_path(pid: u32) -> Option<String> {
    unsafe {
        let mod_snap: SnapshotHandle =
            CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | pid, pid);
        if mod_snap.is_negative() {
            return None;
        }

        let mut me = std::mem::zeroed::<MODULEENTRY32W>();
        me.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;

        let result = if Module32FirstW(mod_snap, &mut me) == TRUE.as_i32() {
            let path = utf16_to_string(&me.szExePath);
            if path.is_empty() { None } else { Some(path) }
        } else {
            None
        };

        windows_sys::Win32::Foundation::CloseHandle(mod_snap);
        result
    }
}

/// Callback for EnumWindows — collect PIDs of visible top-level windows.
struct WindowCollector {
    pids: HashSet<u32>,
}

extern "system" fn enum_windows_proc(hwnd: HWND, param: usize) -> i32 {
    if unsafe { IsWindowVisible(hwnd) } == TRUE.as_i32() {
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid != 0 {
            let collector = param as *mut WindowCollector;
            unsafe {
                if !collector.is_null() {
                    (*collector).pids.insert(pid);
                }
            }
        }
    }
    TRUE.as_i32() // continue enumeration
}

/// Enumerate all visible top-level windows and extract unique app names.
pub fn list_app_names() -> Vec<String> {
    // Ensure a desktop session is active.
    unsafe {
        let _ = GetDesktopWindow();
    }

    let mut collector = WindowCollector {
        pids: HashSet::new(),
    };

    unsafe {
        let callback: unsafe extern "system" fn(HWND, usize) -> i32 =
            enum_windows_proc;
        EnumWindows(Some(callback), &mut collector as *mut _ as usize);
    }

    let mut app_names: Vec<String> = Vec::new();

    for &pid in &collector.pids {
        if let Some(exe_path) = get_process_exe_path(pid) {
            // Match the same logic as active-win-pos-rs: FileDescription
            // first, then file stem as fallback.
            let app_name = get_file_description(&exe_path)
                .unwrap_or_else(|| file_stem(&exe_path));

            if !app_name.is_empty() {
                app_names.push(app_name);
            }
        }
    }

    app_names.sort();
    app_names.dedup();
    app_names
}
