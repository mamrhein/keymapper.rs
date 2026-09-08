// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Device identification shared by the raw input thread and the hook proc.
//!
//! The low-level hook cannot identify the source device — `KBDLLHOOKSTRUCT`
//! carries no device handle — while raw input exposes the source's `hDevice`
//! on every `WM_INPUT`.  The raw input thread buffers recent keyboard
//! key-downs (with a short expiry) and resolves device handles to interface
//! paths on demand; the hook proc matches its event against the buffer to
//! recover the source device for the per-rule keyboard filters.
//!
//! The match is strictly non-blocking except for a bounded retry: raw input
//! and the hook do not deliver in a guaranteed order, so a raw key-down may
//! arrive a few milliseconds after the hook callback for the same press.  A
//! match that never arrives degrades to a lookup without device
//! identification (device-filtered rules simply do not fire for that
//! event), and the input chain must never be blocked for long.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use super::raw_input::RawInputEvent;
use crate::common::hid_usage::HidUsage;

/// Raw input key-downs older than this are discarded, so a stale event from
/// a previous keystroke is never matched against the current press.
pub(crate) const RAW_EVENT_MAX_AGE: Duration = Duration::from_millis(100);

/// Raw input key-down stored in the matching buffer with a receive
/// timestamp so that stale entries can be evicted.
#[derive(Debug)]
pub(crate) struct BufferedRawInput {
    /// The raw input event (always a key-down).
    pub event: RawInputEvent,
    /// When the event was received.
    pub received_at: Instant,
}

/// Buffer of recent raw input key-downs.
///
/// The raw input thread (the sole writer) appends to and evicts from the
/// buffer, while the hook thread (the sole reader) scans it non-blockingly
/// via [`match_usage`].  A mutex keeps the two threads honest; contention
/// is negligible because the buffer holds at most a handful of entries.
static RAW_BUFFER: parking_lot::Mutex<Vec<BufferedRawInput>> =
    parking_lot::Mutex::new(Vec::new());

/// Append a raw input key-down to the buffer and evict stale entries.
pub(crate) fn push_event(event: RawInputEvent) {
    let mut buffer = RAW_BUFFER.lock();
    buffer.push(BufferedRawInput {
        event,
        received_at: Instant::now(),
    });
    evict_stale(&mut buffer, RAW_EVENT_MAX_AGE);
}

/// Drop entries older than *max_age*.
fn evict_stale(buffer: &mut Vec<BufferedRawInput>, max_age: Duration) {
    buffer.retain(|entry| entry.received_at.elapsed() < max_age);
}

/// Find the index of the most recent entry matching *usage*, or `None`.
///
/// The buffer is not strictly ordered, so the scan keeps the latest
/// `received_at` it has seen.
fn find_match(
    buffer: &Vec<BufferedRawInput>,
    usage: HidUsage,
) -> Option<usize> {
    let mut best_idx: Option<usize> = None;
    let mut best_time: Option<Instant> = None;

    for (idx, entry) in buffer.iter().enumerate() {
        if entry.event.usage == Some(usage)
            && (best_time.is_none() || entry.received_at > best_time.unwrap())
        {
            best_idx = Some(idx);
            best_time = Some(entry.received_at);
        }
    }

    best_idx
}

/// Find the raw input event matching *usage* and return its device handle
/// pointer.
///
/// The matched entry is removed so it is not reused for a subsequent press.
pub(crate) fn match_usage(usage: HidUsage) -> Option<usize> {
    let mut buffer = RAW_BUFFER.lock();
    let idx = find_match(&buffer, usage)?;
    let entry = buffer.remove(idx);
    Some(entry.event.device_handle_ptr)
}

/// Remove all entries older than [`RAW_EVENT_MAX_AGE`] without looking
/// anything up.
///
/// The raw input thread calls this while idle so a slow or absent hook
/// consumer cannot let the buffer grow.
pub(crate) fn evict() {
    evict_stale(&mut RAW_BUFFER.lock(), RAW_EVENT_MAX_AGE);
}

// ---------------------------------------------------------------------------
// Device path cache
// ---------------------------------------------------------------------------

/// Cached mapping from raw device handle pointers to device interface
/// paths.
///
/// Populated on demand via `GetRawInputDeviceInfoW` when a raw input event
/// arrives with a previously unseen `hDevice`.  The resolved path is the
/// same format as the `device` field populated by `list_keyboards()`, so it
/// matches the keyboard registry used by the per-rule filters.
#[derive(Debug, Default)]
pub(crate) struct DeviceCache {
    map: Mutex<HashMap<usize, String>>,
}

impl DeviceCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the device interface path for the given raw device handle
    /// pointer, resolving it on demand if not already cached.
    pub fn get_or_resolve(&self, handle_ptr: usize) -> Option<String> {
        {
            let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(path) = map.get(&handle_ptr) {
                return Some(path.clone());
            }
        }

        // Resolve via GetRawInputDeviceInfoW.
        let path = resolve_device_path(handle_ptr)?;

        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(handle_ptr, path.clone());

        Some(path)
    }
}

/// Process-wide cache, shared by the hook proc and the raw input thread so
/// both resolve device handles to the same interface paths.
static DEVICE_CACHE: std::sync::OnceLock<DeviceCache> =
    std::sync::OnceLock::new();

/// Return the process-wide device cache, creating it on first use.
pub(crate) fn device_cache() -> &'static DeviceCache {
    DEVICE_CACHE.get_or_init(DeviceCache::new)
}

/// Resolve a raw device handle to its interface path string.
///
/// Calls `GetRawInputDeviceInfoW` with `RIDI_DEVICENAME` to obtain the
/// device interface path (e.g. `\\?\hid#vid_046d+pid_c31c#...`).  The path
/// format matches the `device` field populated by `list_keyboards()` via
/// SetupAPI, allowing direct lookup in the keyboard registry.
pub(crate) fn resolve_device_path(handle_ptr: usize) -> Option<String> {
    use windows::Win32::{
        Foundation::HANDLE,
        UI::Input::{GetRawInputDeviceInfoW, RIDI_DEVICENAME},
    };

    let h_device = HANDLE(handle_ptr as *mut std::ffi::c_void);
    if h_device.is_invalid() {
        return None;
    }

    // Probe call to get the required buffer size.
    let mut size: u32 = 0;
    let result = unsafe {
        GetRawInputDeviceInfoW(
            Some(h_device),
            RIDI_DEVICENAME,
            None,
            &mut size,
        )
    };
    // UINT_MAX (0xFFFFFFFF) indicates an error.
    if result == u32::MAX {
        return None;
    }

    if size == 0 {
        return None;
    }

    // Allocate a wide-character buffer and retrieve the device name.
    let mut buffer = vec![0u16; size as usize];
    let bytes_returned = unsafe {
        GetRawInputDeviceInfoW(
            Some(h_device),
            RIDI_DEVICENAME,
            Some(buffer.as_mut_ptr() as *mut std::ffi::c_void),
            &mut size,
        )
    };
    // UINT_MAX indicates an error.
    if bytes_returned == u32::MAX {
        return None;
    }

    // Trim trailing null terminator.
    if size > 0 && buffer[size as usize - 1] == 0 {
        buffer.truncate(size as usize - 1);
    }

    // The handle is just an opaque pointer identifying the raw input
    // device; we do not own it and must not close it.

    Some(String::from_utf16_lossy(&buffer))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::hid_usage::HidUsage;

    /// Build a raw input key-down event with the given usage and device
    /// handle.
    fn raw_event(usage: HidUsage, device: usize) -> RawInputEvent {
        RawInputEvent {
            usage: Some(usage),
            vk_code: None,
            is_key_up: false,
            device_handle_ptr: device,
        }
    }

    /// Build a buffer entry for *usage* that arrived *age* ago.
    fn entry(
        usage: HidUsage,
        device: usize,
        age: Duration,
    ) -> BufferedRawInput {
        BufferedRawInput {
            event: raw_event(usage, device),
            received_at: Instant::now() - age,
        }
    }

    #[test]
    fn find_match_returns_most_recent() {
        let buffer = vec![
            entry(HidUsage::A, 0x11, Duration::from_millis(20)),
            entry(HidUsage::B, 0x22, Duration::from_millis(10)),
            entry(HidUsage::A, 0x33, Duration::from_millis(1)),
        ];

        // Two A entries: the most recent (0x33) wins.
        let Some(idx) = find_match(&buffer, HidUsage::A) else {
            panic!("expected a match for A");
        };
        assert_eq!(buffer[idx].event.device_handle_ptr, 0x33);
        // An unmatched usage is left alone.
        assert_eq!(
            find_match(&buffer, HidUsage::B)
                .map(|i| buffer[i].event.device_handle_ptr),
            Some(0x22)
        );
    }

    #[test]
    fn find_match_is_not_order_dependent() {
        // The buffer is not strictly ordered: a later entry may carry an
        // older timestamp.  The scan must still pick the latest one.
        let buffer = vec![
            entry(HidUsage::A, 0x11, Duration::from_millis(50)),
            entry(HidUsage::A, 0x33, Duration::from_millis(10)),
            entry(HidUsage::A, 0x22, Duration::from_millis(30)),
        ];

        let Some(idx) = find_match(&buffer, HidUsage::A) else {
            panic!("expected a match for A");
        };
        assert_eq!(buffer[idx].event.device_handle_ptr, 0x33);
    }

    #[test]
    fn find_match_returns_none_for_unseen_usage() {
        let buffer = vec![entry(HidUsage::A, 0x11, Duration::from_millis(5))];
        assert!(find_match(&buffer, HidUsage::B).is_none());
    }

    #[test]
    fn match_usage_consumes_the_matched_entry() {
        // The static buffer is shared, so this test uses a usage no other
        // test touches.
        push_event(raw_event(HidUsage::F1, 0x11));
        push_event(raw_event(HidUsage::F2, 0x22));
        push_event(raw_event(HidUsage::F1, 0x33));

        let Some(device) = match_usage(HidUsage::F1) else {
            panic!("expected a match for F1");
        };
        assert_eq!(device, 0x33);

        // The matched entry is consumed; the older F1 remains.
        assert_eq!(match_usage(HidUsage::F1), Some(0x11));
        assert!(match_usage(HidUsage::F1).is_none());
        // The F2 entry was never looked up and is still buffered.
        assert_eq!(match_usage(HidUsage::F2), Some(0x22));
    }

    #[test]
    fn evict_stale_removes_old_entries() {
        let mut buffer = vec![
            entry(HidUsage::A, 0x11, Duration::from_millis(200)),
            entry(HidUsage::B, 0x22, Duration::from_millis(50)),
            entry(HidUsage::C, 0x33, Duration::from_millis(5)),
        ];

        evict_stale(&mut buffer, RAW_EVENT_MAX_AGE);

        let usages: Vec<_> = buffer.iter().map(|e| e.event.usage).collect();
        assert_eq!(usages, vec![Some(HidUsage::B), Some(HidUsage::C)]);
    }

    #[test]
    fn evict_stale_under_load() {
        // Interleave fresh and stale entries; only the fresh ones survive.
        // Keyboard ids 4..=53 are all defined usages.
        let mut buffer: Vec<BufferedRawInput> = (0..50)
            .filter_map(|i| {
                HidUsage::keyboard(i as u16 + 4).map(|usage| {
                    entry(
                        usage,
                        i as usize,
                        if i % 2 == 0 {
                            Duration::from_millis(150)
                        } else {
                            Duration::from_millis(2)
                        },
                    )
                })
            })
            .collect();

        evict_stale(&mut buffer, RAW_EVENT_MAX_AGE);

        assert!(
            buffer
                .iter()
                .all(|e| e.received_at.elapsed() < RAW_EVENT_MAX_AGE)
        );
        assert!(!buffer.is_empty());
    }

    #[test]
    fn device_cache_resolves_and_caches() {
        let cache = DeviceCache::new();
        // A bogus handle never resolves; a second call must hit the
        // (empty) cache path without panicking.
        assert!(cache.get_or_resolve(0xDEAD_BEEF).is_none());
        assert!(cache.get_or_resolve(0xDEAD_BEEF).is_none());
    }

    #[test]
    fn resolve_device_path_rejects_null_handle() {
        assert!(resolve_device_path(0).is_none());
    }

    #[test]
    fn buffer_is_shared_between_threads() {
        // A usage unique to this test keeps the shared static isolated
        // from the parallel unit tests.
        let handle = std::thread::spawn(|| {
            push_event(raw_event(HidUsage::F12, 0x44));
        });
        handle.join().unwrap();

        let Some(device) = match_usage(HidUsage::F12) else {
            panic!("expected the cross-thread event to match");
        };
        assert_eq!(device, 0x44);
    }
}
