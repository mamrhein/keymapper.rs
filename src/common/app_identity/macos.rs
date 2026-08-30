// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! macOS application identity.
//!
//! The foreground application is queried via NSWorkspace.  The visible
//! application list is read from the CoreGraphics window list; the returned
//! `app_name` is the value of `kCGWindowOwnerName` from
//! `CGWindowListCopyWindowInfo`, which is exactly what `active-win-pos-rs`
//! returns for the active window on macOS.

use std::{ffi::c_void, ptr::NonNull};

use objc2_app_kit::NSWorkspace;
use objc2_core_foundation::{
    CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_graphics::{
    CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID,
};

/// Synchronously query the current foreground application name.
///
/// Returns `"unknown"` if no application is in the foreground or the
/// query fails.
pub fn get_active_app_name() -> String {
    // NSWorkspace is a Foundation singleton that is safe to access from
    // any thread.  The subsequent calls only read immutable state from the
    // window server.
    let workspace = NSWorkspace::sharedWorkspace();

    // Extract the frontmost application outside the let-else to satisfy
    // Rust 2024 edition restrictions on unsafe blocks in pattern guards.
    let maybe_app = workspace.frontmostApplication();
    let Some(app) = maybe_app else {
        return "unknown".to_string();
    };

    // Prefer the localized display name; fall back to the bundle
    // identifier if the display name is unavailable.
    if let Some(name) = app.localizedName() {
        return name.to_string();
    }

    if let Some(bundle_id) = app.bundleIdentifier() {
        return bundle_id.to_string();
    }

    "unknown".to_string()
}

// ---------------------------------------------------------------------------
// Visible application list (CoreGraphics window list)
// ---------------------------------------------------------------------------

/// Internal record used for deduplication.
struct WindowInfo {
    pid: u64,
    app_name: String,
}

/// Try to extract a numeric value for `key` from a dictionary.
fn get_number(dict: &CFDictionary, key: &str) -> Option<i64> {
    let cf_key = CFString::from_str(key);

    let value = get_value_from_dict(dict, &cf_key)?;

    // Downcast to CFNumber.
    let cf_number = value.downcast::<CFNumber>().ok()?;

    // Try SInt64 first, then fall back to SInt32. These are the types
    // used by CG window info dictionaries.
    if let Some(val) = cf_number.as_i64() {
        Some(val)
    } else {
        cf_number.as_i32().map(|val| val as i64)
    }
}

/// Try to extract a string value for `key` from a dictionary.
fn get_string(dict: &CFDictionary, key: &str) -> Option<String> {
    let cf_key = CFString::from_str(key);

    let value = get_value_from_dict(dict, &cf_key)?;

    // Downcast to CFString.
    let cf_string = value.downcast::<CFString>().ok()?;

    let rust_str = cf_string.to_string();
    if rust_str.is_empty() {
        None
    } else {
        Some(rust_str)
    }
}

/// Get a value from a CFDictionary<Opaque, Opaque> as a CFRetained<CFType>.
///
/// Uses the raw-pointer `value_if_present` API because the generic `get()`
/// requires typed dictionaries.
fn get_value_from_dict(
    dict: &CFDictionary,
    key: &CFString,
) -> Option<CFRetained<CFType>> {
    unsafe {
        let mut out_ptr: *const c_void = std::ptr::null();
        let found = dict.value_if_present(
            key as *const CFString as *const c_void,
            &mut out_ptr,
        );

        if !found || out_ptr.is_null() {
            return None;
        }

        // The dictionary owns this pointer, so we retain it.
        Some(CFRetained::retain(NonNull::new_unchecked(
            out_ptr as *mut CFType,
        )))
    }
}

/// Enumerate all on-screen windows and extract unique application names.
pub fn list_app_names() -> Vec<String> {
    let options = CGWindowListOption::OptionOnScreenOnly
        | CGWindowListOption::ExcludeDesktopElements;

    let Some(array) = CGWindowListCopyWindowInfo(options, kCGNullWindowID)
    else {
        return Vec::new();
    };

    list_from_array(&array)
}

/// Extract app names from a CFArray of window dictionaries.
fn list_from_array(
    array: &CFRetained<objc2_core_foundation::CFArray>,
) -> Vec<String> {
    // Dereference to &CFArray for the FFI-style accessor functions.
    let cf_array: &objc2_core_foundation::CFArray = array;

    unsafe {
        let count = cf_array.count();

        let mut seen: Vec<WindowInfo> = Vec::new();

        for i in 0..count {
            let value_ptr = cf_array.value_at_index(i);

            if value_ptr.is_null() {
                continue;
            }

            // The array owns this pointer, so we retain it.
            let cf_type = CFRetained::retain(NonNull::new_unchecked(
                value_ptr as *mut CFType,
            ));

            // Check that it's a CFDictionary.
            if cf_type.downcast_ref::<CFDictionary>().is_none() {
                continue;
            }

            let dict = cf_type.downcast::<CFDictionary>().unwrap();

            // Get window owner PID — skip if missing or zero.
            let Some(pid) = get_number(&dict, "kCGWindowOwnerPID") else {
                continue;
            };
            if pid == 0 {
                continue;
            }

            // Get the application name — skip if missing or empty.
            let Some(app_name) = get_string(&dict, "kCGWindowOwnerName")
            else {
                continue;
            };

            seen.push(WindowInfo {
                pid: pid as u64,
                app_name,
            });
        }

        // Deduplicate: sort by name then pid, keep first occurrence of each
        // unique name.
        seen.sort_by(|a, b| {
            a.app_name.cmp(&b.app_name).then(a.pid.cmp(&b.pid))
        });
        seen.dedup_by(|a, b| a.app_name == b.app_name && a.pid == b.pid);

        seen.into_iter().map(|info| info.app_name).collect()
    }
}
