// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows application identity.
//!
//! Both entry points resolve each process to the same canonical name: the
//! file name of the process's main executable (e.g. `pwsh.exe`), which
//! uniquely identifies the window-owning process, is stable across app
//! updates, and is independent of the system locale.  The `FileDescription`
//! from the PE version resources serves only as a human-readable display
//! alias.  `get_active_app_name` resolves the process that owns the
//! foreground window, and `list_app_names` enumerates the processes that
//! own visible top-level windows.  This guarantees the active app name is
//! always one of the names printed by `keymapper appnames`.

use std::{collections::HashSet, path::Path};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HWND, LPARAM, RECT},
        System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        },
        UI::WindowsAndMessaging::{
            EnumWindows, GetDesktopWindow, GetForegroundWindow, GetWindowRect,
            GetWindowThreadProcessId, IsWindowVisible,
        },
    },
    core::{BOOL, PWSTR},
};

use super::AppName;

/// Synchronously query the current foreground application name.
///
/// Resolves the foreground window to its owning process and returns the
/// same canonical name [`list_app_names`] produces for that process.
/// Returns `"unknown"` when the query fails or no window is in the
/// foreground.
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

    app_identity_for_pid(pid)
        .map(|entry| entry.name)
        .unwrap_or_else(|| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// Visible application list (EnumWindows + PE version resources)
// ---------------------------------------------------------------------------

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

    if !unsafe {
        windows::Win32::Storage::FileSystem::VerQueryValueW(
            buffer.as_ptr() as _,
            windows::core::PCWSTR(sub_block_utf16.as_ptr()),
            &mut lplp_buffer as *const _ as *mut _,
            &mut pu_len,
        )
    }
    .as_bool()
    {
        return None;
    }

    if pu_len == 0 || lplp_buffer.is_null() {
        return None;
    }

    Some(unsafe {
        std::slice::from_raw_parts(lplp_buffer as *const u16, pu_len as usize)
            .to_vec()
    })
}

/// Resolve the actual language-specific `FileDescription` sub-block path by
/// reading the translation table from the version resource.
unsafe fn resolve_file_description_path(buffer: &[u8]) -> Option<String> {
    let lang_data =
        unsafe { ver_query_value(buffer, "\\VarFileInfo\\Translation") }?;
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
            windows::Win32::Storage::FileSystem::GetFileVersionInfoSizeW(
                windows::core::PCWSTR::from_raw(path_utf16.as_ptr()),
                None,
            );
        if size == 0 {
            return None;
        }

        let mut buffer = vec![0u8; size as usize];
        if windows::Win32::Storage::FileSystem::GetFileVersionInfoW(
            windows::core::PCWSTR::from_raw(path_utf16.as_ptr()),
            None,
            size,
            buffer.as_mut_ptr() as _,
        )
        .is_err()
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
        if let Some(sub_block) = resolve_file_description_path(&buffer)
            && let Some(desc) = ver_query_value(&buffer, &sub_block)
        {
            let s = utf16_to_string(&desc);
            if !s.is_empty() {
                return Some(s);
            }
        }

        None
    }
}

/// Extract the file name with extension from a path (e.g., `chrome.exe`
/// from `C:\Program Files\Google\Chrome\Application\chrome.exe`).
fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// Get the full path of the process's main executable.
fn get_process_image_path(pid: u32) -> Option<String> {
    let Ok(handle) = (unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
    }) else {
        return None;
    };

    // Image paths rarely exceed 1024 UTF-16 units; retry with a much larger
    // buffer when the path is longer than that.
    let result = [1024usize, 32_768].into_iter().find_map(|capacity| {
        let mut buffer = vec![0u16; capacity];
        let mut size = capacity as u32;

        if (unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut size,
            )
        })
        .is_err()
        {
            return None;
        }

        let path = utf16_to_string(&buffer[..size as usize]);
        (!path.is_empty()).then_some(path)
    });

    // CloseHandle fails only with an invalid handle, which would be a bug.
    let _ = unsafe { CloseHandle(handle) };
    result
}

/// Callback for EnumWindows — collect PIDs of visible top-level windows.
struct WindowCollector {
    pids: HashSet<u32>,
}

/// Callback for EnumWindows — collect PIDs of visible top-level windows.
extern "system" fn enum_windows_proc(hwnd: HWND, param: LPARAM) -> BOOL {
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        // Skip zero-area helper windows (e.g. the 0x0 ConPTY
        // "PseudoConsoleWindow" that PowerShell creates in-process).  Such
        // windows can never take the foreground, so their owning process
        // could never be the active app.  Minimized app windows are
        // unaffected: they still report a non-zero, off-screen rect.
        let mut rect = unsafe { core::mem::zeroed::<RECT>() };
        let has_area = unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok()
            && rect.right - rect.left != 0
            && rect.bottom - rect.top != 0;

        if has_area {
            let mut pid: u32 = 0;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid != 0 {
                let collector = param.0 as *mut WindowCollector;
                unsafe {
                    if !collector.is_null() {
                        (*collector).pids.insert(pid);
                    }
                }
            }
        }
    }
    BOOL(1) // continue enumeration
}

/// Enumerate all visible, non-zero-area top-level windows and extract
/// unique application entries.
pub fn list_app_names() -> Vec<AppName> {
    // Ensure a desktop session is active.
    unsafe {
        let _ = GetDesktopWindow();
    };

    let mut collector = WindowCollector {
        pids: HashSet::new(),
    };

    // EnumWindows returns FALSE on failure, which leaves the collector
    // partially populated.  This is tolerated — the caller simply gets
    // fewer app names.
    unsafe {
        let _ = EnumWindows(
            Some(enum_windows_proc),
            LPARAM(&mut collector as *const _ as isize),
        );
    };

    let mut apps: Vec<AppName> = Vec::new();

    for &pid in &collector.pids {
        if let Some(entry) = app_identity_for_pid(pid) {
            apps.push(entry);
        }
    }

    apps.sort_by(|a, b| a.name.cmp(&b.name));
    apps.dedup_by(|a, b| a.name == b.name);
    apps
}

/// Resolve the canonical application identity for a process: the file name
/// of its main executable (the rule-matching key), plus the
/// `FileDescription` from its PE version resources as a human-readable
/// display alias.
///
/// This is the single definition of the per-process app identity on
/// Windows.  Both entry points of this module must go through it so they
/// stay in the same namespace.
fn app_identity_for_pid(pid: u32) -> Option<AppName> {
    let image_path = get_process_image_path(pid)?;
    let name = file_name(&image_path);
    if name.is_empty() {
        return None;
    }

    let display = get_file_description(&image_path).unwrap_or(name.clone());
    Some(AppName { name, display })
}
x
x
