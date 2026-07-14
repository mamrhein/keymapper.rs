// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Lists visible application names using CoreGraphics.
//!
//! The returned `app_name` is the value of `kCGWindowOwnerName` from
//! `CGWindowListCopyWindowInfo`, which is exactly what `active-win-pos-rs`
//! returns for the active window on macOS.

use std::ffi::c_void;

use core_foundation::{
    array::CFArrayGetTypeID,
    base::{CFGetTypeID, CFRelease, TCFType},
    dictionary::CFDictionaryGetTypeID,
    number::CFNumberGetTypeID,
    string::{CFString, CFStringGetTypeID},
};
use core_graphics::{
    display::{
        CFArrayRef, CFDictionaryGetValueIfPresent, CFDictionaryRef,
        CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID,
    },
    window::{
        kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly,
    },
};

/// Internal record used for deduplication.
struct WindowInfo {
    pid: u64,
    app_name: String,
}

/// CoreFoundation number type constants and FFI.
#[allow(non_upper_case_globals)]
mod cf_number {
    use std::ffi::c_void;

    use core_foundation::number::CFNumberType;

    // SInt32 and SInt64 are the types used by CG window info.
    pub const kCFNumberSInt32Type: CFNumberType = 3;
    pub const kCFNumberSInt64Type: CFNumberType = 4;

    unsafe extern "C" {
        #[link_name = "CFNumberGetType"]
        pub fn CFNumberGetType(num: *const c_void) -> CFNumberType;

        #[link_name = "CFNumberGetValue"]
        pub fn CFNumberGetValue(
            num: *const c_void,
            theType: CFNumberType,
            valuePtr: *mut c_void,
        ) -> bool;
    }
}

/// Try to extract a numeric value for `key` from a CFDictionaryRef.
fn get_number(dict: CFDictionaryRef, key: &str) -> Option<i64> {
    let cf_key = CFString::new(key);

    unsafe {
        let mut out_value: *const c_void = std::ptr::null();
        let found = CFDictionaryGetValueIfPresent(
            dict,
            cf_key.as_concrete_TypeRef() as *const c_void,
            &mut out_value,
        );

        if found == 0 || out_value.is_null() {
            return None;
        }

        // Check that it's a CFNumber.
        if CFGetTypeID(out_value) != CFNumberGetTypeID() {
            return None;
        }

        let num_type = cf_number::CFNumberGetType(out_value);

        // Try SInt64 first.
        if num_type == cf_number::kCFNumberSInt64Type {
            let mut out: i64 = 0;
            if cf_number::CFNumberGetValue(
                out_value,
                num_type,
                &mut out as *mut _ as *mut c_void,
            ) {
                return Some(out);
            }
        }

        // Try SInt32.
        if num_type == cf_number::kCFNumberSInt32Type {
            let mut out: i32 = 0;
            if cf_number::CFNumberGetValue(
                out_value,
                num_type,
                &mut out as *mut _ as *mut c_void,
            ) {
                return Some(out as i64);
            }
        }

        None
    }
}

/// Try to extract a string value for `key` from a CFDictionaryRef.
fn get_string(dict: CFDictionaryRef, key: &str) -> Option<String> {
    let cf_key = CFString::new(key);

    unsafe {
        let mut out_value: *const c_void = std::ptr::null();
        let found = CFDictionaryGetValueIfPresent(
            dict,
            cf_key.as_concrete_TypeRef() as *const c_void,
            &mut out_value,
        );

        if found == 0 || out_value.is_null() {
            return None;
        }

        // Check that it's a CFString.
        if CFGetTypeID(out_value) != CFStringGetTypeID() {
            return None;
        }

        // Wrap the raw pointer to extract the string.  We use
        // wrap_under_get_rule because we don't own a retain — the
        // dictionary does.
        let s = CFString::wrap_under_get_rule(out_value as *mut _);
        let rust_str = s.to_string();

        if rust_str.is_empty() {
            None
        } else {
            Some(rust_str)
        }
    }
}

/// Enumerate all on-screen windows and extract unique application names.
pub fn list_app_names() -> Vec<String> {
    unsafe {
        let options: CGWindowListOption = kCGWindowListOptionOnScreenOnly
            | kCGWindowListExcludeDesktopElements;

        let raw_array: CFArrayRef =
            CGWindowListCopyWindowInfo(options, kCGNullWindowID);

        if raw_array.is_null() {
            return Vec::new();
        }

        // Verify it's actually a CFArray.
        if CFGetTypeID(raw_array as *const c_void) != CFArrayGetTypeID() {
            CFRelease(raw_array as *const c_void);
            return Vec::new();
        }

        let result = list_from_array(raw_array);
        CFRelease(raw_array as *const c_void);
        result
    }
}

/// Extract app names from a CFArrayRef of window dictionaries.
fn list_from_array(raw_array: CFArrayRef) -> Vec<String> {
    use core_foundation_sys::array::{
        CFArrayGetCount, CFArrayGetValueAtIndex,
    };

    unsafe {
        let count = CFArrayGetCount(raw_array);

        let mut seen: Vec<WindowInfo> = Vec::new();

        for i in 0..count {
            let value_ptr = CFArrayGetValueAtIndex(raw_array, i);

            if value_ptr.is_null() {
                continue;
            }

            let dict = value_ptr as CFDictionaryRef;

            // Check it's a CFDictionary.
            if CFGetTypeID(dict as *const c_void) != CFDictionaryGetTypeID() {
                continue;
            }

            // Get window owner PID — skip if missing or zero.
            let Some(pid) = get_number(dict, "kCGWindowOwnerPID") else {
                continue;
            };
            if pid == 0 {
                continue;
            }

            // Get the application name — skip if missing or empty.
            let Some(app_name) = get_string(dict, "kCGWindowOwnerName") else {
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
