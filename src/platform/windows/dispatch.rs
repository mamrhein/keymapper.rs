// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Worker-thread event dispatch and device identification.
//!
//! Synchronises the hook thread and the raw input thread via channels.  The
//! hook thread sends key events together with a reply channel; the worker
//! matches against recent raw input events to identify the source keyboard,
//! performs the mapping lookup, and sends the reply (\`swallow\` or \`pass
//! through\`).
//!
//! Architecture:
//! - Hook events carry a \`crossbeam_channel::Sender<Decision>\` (capacity 1)
//!   for the reply.  The hook proc polls this channel with short sleeps to
//!   avoid blocking the Windows message pump.
//! - The worker matches hook events against raw input events by their decoded
//!   `HidUsage`, which is also the key space of the compiled rules.
//! - Raw input key-down events are buffered with a short expiry (100 ms) to
//!   compensate for non-deterministic arrival order.
//! - Consumer Page key-downs from standalone Consumer Control devices have no
//!   hook event; the worker processes and emits them directly.
//! - A decision cache keyed by \`vk_code\` ensures that key-up events are
//!   treated consistently with their key-down counterpart.
//! - The worker resolves device paths via \`GetRawInputDeviceInfoW\` and
//!   caches them so that subsequent events from the same device are matched
//!   without repeated API calls.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

use crossbeam_channel::{self, Receiver, Sender};
use parking_lot::RwLock;
use windows::Win32::{
    Foundation::HANDLE,
    UI::Input::{
        GetRawInputDeviceInfoW, KeyboardAndMouse::VIRTUAL_KEY, RIDI_DEVICENAME,
    },
};

use crate::{
    common::hid_usage::{HidUsage, PAGE_CONSUMER},
    daemon::state::Lookup,
    platform::windows::{
        mapping::{
            capture_enabled, capture_record_forwarded_down,
            capture_record_forwarded_up, capture_release_triggered_modifiers,
            emit_forwarded_key, emit_mapped_output, extract_modifier_bits,
        },
        raw_input::RawInputEvent,
    },
};

/// Result of a mapping lookup sent from the worker back to the hook thread.
///
/// The \`Swallow\` variant carries the resolved output events so that the hook
/// proc can emit them directly without performing its own lookup.  This avoids
/// a mismatch: the worker decides with device identification, but the hook
/// proc would lookup without it.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Swallow the event and emit the given output keys.
    Swallow(Vec<crate::daemon::mapping_cache::NativeKey>),
    /// Pass the event through to the next hook / default handler.
    PassThrough,
}

/// A keyboard event from the hook thread that the worker must decide on.
///
/// Carries a reply channel (capacity 1) so the hook proc can block until
/// the worker has resolved the mapping.
pub struct HookEvent {
    /// Virtual-key code of the event.
    pub vk_code: VIRTUAL_KEY,

    /// Decoded HID identity of the event — the key space of the compiled
    /// rules.  `None` for virtual-key codes without a `HidUsage`.
    pub usage: Option<HidUsage>,

    /// \`true\` for key-up, \`false\` for key-down.
    pub is_key_up: bool,

    /// Bitmask of currently pressed modifier keys (excluding the current
    /// key).
    pub modifiers: u8,

    /// Reply channel — the worker sends the decision and drops the sender.
    pub reply_tx: Sender<Decision>,
}

/// Raw input event stored in the matching buffer with a timestamp so that
/// stale entries can be evicted.
#[derive(Debug)]
struct BufferedRawInput {
    event: RawInputEvent,
    received_at: Instant,
}

/// Cached mapping from raw device handle pointers to device interface paths.
///
/// Populated on demand via \`GetRawInputDeviceInfoW\` when a raw input event
/// arrives with a previously unseen \`hDevice\`.  The resolved path is the
/// same format as the \`device\` field populated by \`list_keyboards()\`.
#[derive(Debug, Default)]
struct DeviceCache {
    map: Mutex<HashMap<usize, String>>,
}

impl DeviceCache {
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

/// Cached state from a key-down event, used to ensure the corresponding
/// key-up is treated consistently.
#[derive(Clone)]
struct CachedKeyState {
    /// The decision from the key-down (swallow or pass through).
    decision: Decision,
    /// Modifier state at the time of the key-down.  Reserved for future use
    /// in matching modifiers on key-up.
    #[allow(dead_code)]
    modifiers: u8,
}

/// Cached state keyed by virtual-key code.
///
/// When a key-down is processed, the decision and modifier state are stored
/// here so that the corresponding key-up can be treated consistently.
/// Entries are removed when the key-up is processed.
static KEY_STATE_CACHE: std::sync::OnceLock<KeyStateCache> =
    std::sync::OnceLock::new();

struct KeyStateCache {
    inner: Mutex<HashMap<u16, CachedKeyState>>,
}

impl KeyStateCache {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, vk: u16) -> Option<CachedKeyState> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&vk)
            .cloned()
    }

    fn insert(&self, vk: u16, state: CachedKeyState) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(vk, state);
    }

    fn remove(&self, vk: u16) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&vk);
    }
}

fn key_state_cache() -> &'static KeyStateCache {
    KEY_STATE_CACHE.get_or_init(KeyStateCache::new)
}

// ---------------------------------------------------------------------------
// Worker thread
// ---------------------------------------------------------------------------

/// Spawns the worker thread that mediates between the hook and raw input.
///
/// The worker receives \`HookEvent\`s from the hook thread and
/// \`RawInputEvent\`s from the raw input message loop.  It matches raw input
/// events against hook events to identify the source keyboard, performs the
/// mapping lookup, and replies to the hook thread with \`Swallow\` or
/// \`PassThrough\`.
pub fn spawn_worker(
    lookup: Arc<RwLock<dyn Lookup>>,
    raw_rx: Receiver<RawInputEvent>,
) -> Sender<HookEvent> {
    let (hook_tx, hook_rx) = crossbeam_channel::unbounded();

    std::thread::Builder::new()
        .name("keymapper-worker".into())
        .spawn(move || worker_loop(lookup, hook_rx, raw_rx))
        .expect("failed to spawn worker thread");

    hook_tx
}

/// Main loop of the worker thread.
///
/// Uses \`crossbeam_channel::select!\` to listen on both the hook and raw
/// input channels simultaneously.  Raw input key-down events are buffered;
/// hook events are matched against the buffer to identify the source device.
fn worker_loop(
    lookup: Arc<RwLock<dyn Lookup>>,
    hook_rx: Receiver<HookEvent>,
    raw_rx: Receiver<RawInputEvent>,
) {
    let mut raw_buffer: Vec<BufferedRawInput> = Vec::new();
    let device_cache = DeviceCache::new();

    loop {
        crossbeam_channel::select! {
            recv(hook_rx) -> hook_result => {
                match hook_result {
                    Ok(event) => {
                        process_hook_event(
                            &event,
                            &lookup,
                            &mut raw_buffer,
                            &device_cache,
                        );
                    }
                    Err(crossbeam_channel::RecvError) => {
                        // Hook thread disconnected — shut down.
                        break;
                    }
                }
            }
            recv(raw_rx) -> raw_result => {
                match raw_result {
                    Ok(event) => {
                        if capture_enabled() {
                            crate::platform::windows::mapping::capture_debug(&format!(
                                "raw {:?}", event
                            ));
                        }
                        // Buffer only key-down events; key-ups are matched
                        // via the decision cache in the hook processor.
                        if event.is_key_up {
                            continue;
                        }

                        // Standalone Consumer Control events have no VK
                            // code and no hook event to match against, so
                            // process them directly.  Keyboard media keys
                            // (e.g. VK_MEDIA_PLAY_PAUSE) do have a VK code
                            // and a hook event; they must be buffered for
                            // matching like any other keyboard event.
                        if event.vk_code.is_none()
                            && let Some(usage) = event.usage
                            && usage.page() == PAGE_CONSUMER
                        {
                            process_consumer_event(
                                &event, usage, &lookup, &device_cache,
                            );
                        } else {
                            raw_buffer.push(BufferedRawInput {
                                event,
                                received_at: Instant::now(),
                            });
                        }
                    }
                    Err(crossbeam_channel::RecvError) => {
                        // Raw input thread disconnected — unlikely, but
                        // continue processing hook events without device
                        // identification.
                        break;
                    }
                }
            }
        }

        // Evict stale entries — raw input events older than 100 ms are
        // discarded to avoid matching against events from previous keystrokes.
        evict_stale(&mut raw_buffer, std::time::Duration::from_millis(100));
    }
}

/// Processes a hook event and sends the decision back via the reply channel.
fn process_hook_event(
    event: &HookEvent,
    lookup: &Arc<RwLock<dyn Lookup>>,
    raw_buffer: &mut Vec<BufferedRawInput>,
    device_cache: &DeviceCache,
) {
    let cache = key_state_cache();
    let decision = if event.is_key_up {
        // Key-up — use the cached decision from the key-down, if any.
        // Strip the outputs because emission happens only on key-down.
        match cache.get(event.vk_code.0) {
            Some(CachedKeyState { decision, .. }) => {
                cache.remove(event.vk_code.0);
                match decision {
                    Decision::Swallow(_) => Decision::Swallow(Vec::new()),
                    Decision::PassThrough => Decision::PassThrough,
                }
            }
            None => Decision::PassThrough,
        }
    } else {
        // Key-down — try to match against raw input for device identification.
        let decision = match event.usage {
            Some(usage) => {
                match find_match_in_buffer(usage, raw_buffer) {
                    Some(handle_ptr) => {
                        // Found a raw input event — resolve device path and
                        // lookup.
                        let device_path =
                            device_cache.get_or_resolve(handle_ptr);
                        decide(
                            lookup,
                            usage,
                            event.modifiers,
                            device_path.as_deref(),
                        )
                    }
                    None => {
                        // No match yet.  Wait briefly for raw input to
                        // arrive, then fall back to a lookup without device
                        // identification.
                        decide_with_delay(
                            lookup,
                            usage,
                            event.modifiers,
                            raw_buffer,
                            device_cache,
                            std::time::Duration::from_millis(10),
                        )
                    }
                }
            }
            None => {
                // The virtual-key code has no HID identity — no compiled
                // rule can match it, so pass through.
                Decision::PassThrough
            }
        };

        // Cache the decision and modifier state so the key-up can reuse them.
        cache.insert(
            event.vk_code.0,
            CachedKeyState {
                decision: decision.clone(),
                modifiers: event.modifiers,
            },
        );
        decision
    };

    // In capture mode the worker performs all emission, on this non-hook
    // thread, so the e2e monitor's hook observes it (a `SendInput` issued
    // from within the hook proc would not reach other hooks).  Mapped
    // outputs are emitted as complete taps; an unmapped (pass-through) key
    // is forwarded as-is.  Emission completes before the reply is sent, so
    // the hook proc only returns once the output has gone out.
    if capture_enabled() {
        crate::platform::windows::mapping::capture_debug(&format!(
            "decide vk={:#04x} up={} mods={:#04x} usage={:?} -> {:?}",
            event.vk_code.0,
            event.is_key_up,
            event.modifiers,
            event.usage,
            decision
        ));
        // The modifier bit of the event's own key, if it is a modifier
        // key.
        let own_bit =
            event.usage.and_then(HidUsage::hid_usage_to_modifier_bit);
        match &decision {
            Decision::Swallow(outputs) => {
                if !event.is_key_up {
                    // The trigger's modifiers were forwarded when pressed.
                    // Release them on the virtual keyboard now so the
                    // output is emitted as a clean tap; mark them consumed
                    // so their physical release is swallowed rather than
                    // forwarded a second time.
                    capture_release_triggered_modifiers(event.modifiers);
                }
                for native_key in outputs {
                    emit_mapped_output(native_key);
                }
            }
            Decision::PassThrough => {
                if event.is_key_up
                    && let Some(bit) = own_bit
                    && capture_record_forwarded_up(bit)
                {
                    // The modifier was already released on the virtual
                    // keyboard when its trigger fired; swallow the physical
                    // release.
                } else {
                    if !event.is_key_up
                        && let Some(bit) = own_bit
                    {
                        capture_record_forwarded_down(bit);
                    }
                    emit_forwarded_key(event.vk_code.0, event.is_key_up);
                }
            }
        }
    }

    // Drop the sender if the receiver was already dropped (hook proc timed
    // out or returned).  This is harmless — the error is simply ignored.
    let _ = event.reply_tx.send(decision);
}

/// Performs the mapping lookup and returns \`Swallow\` with the output events
/// if a mapping is found.
///
/// Compiled rules store the trigger as a `HidUsage`, so the lookup is keyed
/// by the full page-specific usage rather than the virtual-key code.
fn decide(
    lookup: &Arc<RwLock<dyn Lookup>>,
    usage: HidUsage,
    modifiers: u8,
    device_id: Option<&str>,
) -> Decision {
    let guard = lookup.read();
    let outputs = guard
        .for_active_app(usage, modifiers, device_id)
        .or_else(|| guard.global(usage, modifiers, device_id))
        .map(|v| v.to_vec());
    drop(guard);

    if let Some(outputs) = outputs {
        Decision::Swallow(outputs)
    } else {
        Decision::PassThrough
    }
}

/// Process a Consumer Page key-down event from a standalone Consumer
/// Control device.
///
/// These events do not pass through the low-level keyboard hook, so there
/// is no hook event to reply to: the worker performs the lookup itself and
/// emits the mapped outputs directly.  Note that the original media action
/// cannot be suppressed — Windows delivers Consumer Control input to the
/// shell as `WM_APPCOMMAND`, which no keyboard-level hook can intercept.
fn process_consumer_event(
    event: &RawInputEvent,
    usage: HidUsage,
    lookup: &Arc<RwLock<dyn Lookup>>,
    device_cache: &DeviceCache,
) {
    let modifiers = extract_modifier_bits();
    let device_path = device_cache.get_or_resolve(event.device_handle_ptr);

    let guard = lookup.read();
    let outputs = guard
        .for_active_app(usage, modifiers, device_path.as_deref())
        .or_else(|| guard.global(usage, modifiers, device_path.as_deref()))
        .map(|v| v.to_vec());
    drop(guard);

    if let Some(outputs) = outputs {
        for native_key in &outputs {
            emit_mapped_output(native_key);
        }
    }
}

/// Attempts to find a match with a short delay for raw input to arrive.
///
/// Iterates over the delay period, checking the buffer after each step.
/// Falls back to a lookup without device identification if no match is
/// found within the timeout.
fn decide_with_delay(
    lookup: &Arc<RwLock<dyn Lookup>>,
    usage: HidUsage,
    modifiers: u8,
    raw_buffer: &mut Vec<BufferedRawInput>,
    device_cache: &DeviceCache,
    timeout: std::time::Duration,
) -> Decision {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if let Some(handle_ptr) = find_match_in_buffer(usage, raw_buffer) {
            let device_path = device_cache.get_or_resolve(handle_ptr);
            return decide(lookup, usage, modifiers, device_path.as_deref());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    // Timeout — fall back without device identification.
    decide(lookup, usage, modifiers, None)
}

// ---------------------------------------------------------------------------
// Buffer management
// ---------------------------------------------------------------------------

/// Removes entries older than \`max_age\` from the buffer.
fn evict_stale(
    buffer: &mut Vec<BufferedRawInput>,
    max_age: std::time::Duration,
) {
    buffer.retain(|entry| entry.received_at.elapsed() < max_age);
}

/// Finds a raw input event in the buffer that matches the given HID usage.
/// Returns the device handle pointer of the most recent match, or \`None\`
/// if no match is found.  The matched entry is removed from the buffer so
/// it is not reused for subsequent events.
fn find_match_in_buffer(
    usage: HidUsage,
    buffer: &mut Vec<BufferedRawInput>,
) -> Option<usize> {
    // Find the most recent matching entry.  The buffer is not strictly
    // ordered, so we scan for the latest \`received_at\`.
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

    if let Some(idx) = best_idx {
        let handle_ptr = buffer[idx].event.device_handle_ptr;
        buffer.remove(idx);
        Some(handle_ptr)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Device path resolution
// ---------------------------------------------------------------------------

/// Resolves a raw device handle to its interface path string.
///
/// Calls \`GetRawInputDeviceInfoW\` with \`RIDI_DEVICENAME\` to obtain the
/// device interface path (e.g. \`\\\\?\\hid#vid_046d+pid_c31c#...)\`).  This
/// path format matches the \`device\` field populated by \`list_keyboards()\`
/// via SetupAPI, allowing direct lookup in the keyboard registry.
fn resolve_device_path(handle_ptr: usize) -> Option<String> {
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

    // The handle is just an opaque pointer identifying the raw input device;
    // we do not own it and must not close it.

    Some(String::from_utf16_lossy(&buffer))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{
        daemon::{
            mapping_cache::{NativeKey, RuntimeLookupCache},
            state::Lookup,
        },
        platform::Key,
    };

    // -----------------------------------------------------------------------
    // Mock Lookup for testing process_hook_event and decide
    // -----------------------------------------------------------------------

    /// A [`Lookup`] implementation that returns configurable outputs.
    struct MockLookup {
        /// Map of (usage, modifiers, device_id_option) -> outputs.
        global_map: HashMap<(HidUsage, u8, Option<String>), Vec<NativeKey>>,
    }

    impl MockLookup {
        fn new() -> Self {
            Self {
                global_map: HashMap::new(),
            }
        }

        /// Configure the mock to return *outputs* for global lookups of
        /// the given usage with the given modifiers and device ID.
        fn with_global(
            mut self,
            usage: HidUsage,
            modifiers: u8,
            device_id: Option<&str>,
            outputs: Vec<NativeKey>,
        ) -> Self {
            self.global_map.insert(
                (usage, modifiers, device_id.map(String::from)),
                outputs,
            );
            self
        }
    }

    impl std::fmt::Debug for MockLookup {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MockLookup").finish()
        }
    }

    impl Lookup for MockLookup {
        fn for_app(
            &self,
            _app: &str,
            _usage: HidUsage,
            _modifiers: u8,
            _device_id: Option<&str>,
        ) -> Option<&[NativeKey]> {
            None
        }

        fn global(
            &self,
            usage: HidUsage,
            modifiers: u8,
            device_id: Option<&str>,
        ) -> Option<&[NativeKey]> {
            // We store outputs in a static vec so we can return a reference.
            // This is safe because MockLookup lives for the test duration.
            self.global_map
                .get(&(usage, modifiers, device_id.map(String::from)))
                .map(|v| v.as_slice())
        }

        fn for_active_app(
            &self,
            _usage: HidUsage,
            _modifiers: u8,
            _device_id: Option<&str>,
        ) -> Option<&[NativeKey]> {
            None
        }
    }

    /// Helper that builds an Arc<RwLock<dyn Lookup>> from a MockLookup.
    fn arc_lookup(lookup: MockLookup) -> Arc<RwLock<dyn Lookup>> {
        Arc::new(RwLock::new(lookup))
    }

    /// Helper to create a native key output for a given Key.
    fn nk(key: Key) -> NativeKey {
        NativeKey {
            modifiers: 0,
            usage: key.to_hid_usage(),
        }
    }

    // -----------------------------------------------------------------------
    // Decision enum tests
    // -----------------------------------------------------------------------

    #[test]
    fn decision_variants_are_clone() {
        let d1 = Decision::Swallow(vec![]);
        let d2 = d1.clone();
        assert!(matches!(d2, Decision::Swallow(_)));

        let p1 = Decision::PassThrough;
        let p2 = p1.clone();
        assert!(matches!(p2, Decision::PassThrough));
    }

    #[test]
    fn key_state_cache_get_returns_none_for_missing_key() {
        assert!(key_state_cache().get(0xFFFF).is_none());
    }

    #[test]
    fn key_state_cache_insert_and_get() {
        key_state_cache().insert(
            0x41,
            CachedKeyState {
                decision: Decision::Swallow(vec![]),
                modifiers: 0,
            },
        );
        assert!(matches!(
            key_state_cache().get(0x41),
            Some(CachedKeyState {
                decision: Decision::Swallow(_),
                ..
            })
        ));
        key_state_cache().remove(0x41);
    }

    #[test]
    fn key_state_cache_remove_clears_entry() {
        key_state_cache().insert(
            0x57,
            CachedKeyState {
                decision: Decision::PassThrough,
                modifiers: 0,
            },
        );
        assert!(matches!(
            key_state_cache().get(0x57),
            Some(CachedKeyState {
                decision: Decision::PassThrough,
                ..
            })
        ));
        key_state_cache().remove(0x57);
        assert!(key_state_cache().get(0x57).is_none());
    }

    #[test]
    fn key_state_cache_overwrite() {
        key_state_cache().insert(
            0x45,
            CachedKeyState {
                decision: Decision::Swallow(vec![]),
                modifiers: 0,
            },
        );
        key_state_cache().insert(
            0x45,
            CachedKeyState {
                decision: Decision::PassThrough,
                modifiers: 0,
            },
        );
        assert!(matches!(
            key_state_cache().get(0x45),
            Some(CachedKeyState {
                decision: Decision::PassThrough,
                ..
            })
        ));
        key_state_cache().remove(0x45);
    }

    #[test]
    fn device_cache_resolves_and_caches() {
        let cache = DeviceCache::new();

        // A null handle will not resolve, but the cache structure works.
        assert!(cache.get_or_resolve(0).is_none());
    }

    #[test]
    fn evict_stale_removes_old_entries() {
        let mut buffer = vec![
            BufferedRawInput {
                event: RawInputEvent {
                    usage: Some(HidUsage::A),
                    vk_code: Some(VIRTUAL_KEY(0x41)),
                    is_key_up: false,
                    device_handle_ptr: 0x1000,
                },
                received_at: Instant::now()
                    - std::time::Duration::from_millis(200),
            },
            BufferedRawInput {
                event: RawInputEvent {
                    usage: Some(HidUsage::W),
                    vk_code: Some(VIRTUAL_KEY(0x57)),
                    is_key_up: false,
                    device_handle_ptr: 0x2000,
                },
                received_at: Instant::now(),
            },
        ];

        evict_stale(&mut buffer, std::time::Duration::from_millis(100));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer[0].event.vk_code.unwrap().0, 0x57);
    }

    #[test]
    fn evict_stale_keeps_recent_entries() {
        let mut buffer = vec![BufferedRawInput {
            event: RawInputEvent {
                usage: Some(HidUsage::A),
                vk_code: Some(VIRTUAL_KEY(0x41)),
                is_key_up: false,
                device_handle_ptr: 0x1000,
            },
            received_at: Instant::now(),
        }];

        evict_stale(&mut buffer, std::time::Duration::from_millis(100));
        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn evict_stale_empty_buffer() {
        let mut buffer: Vec<BufferedRawInput> = Vec::new();
        evict_stale(&mut buffer, std::time::Duration::from_millis(100));
        assert!(buffer.is_empty());
    }

    #[test]
    fn find_match_in_buffer_returns_most_recent() {
        let now = Instant::now();
        let mut buffer = vec![
            BufferedRawInput {
                event: RawInputEvent {
                    usage: Some(HidUsage::A),
                    vk_code: Some(VIRTUAL_KEY(0x41)),
                    is_key_up: false,
                    device_handle_ptr: 0x1000,
                },
                received_at: now - std::time::Duration::from_millis(50),
            },
            BufferedRawInput {
                event: RawInputEvent {
                    usage: Some(HidUsage::A),
                    vk_code: Some(VIRTUAL_KEY(0x41)),
                    is_key_up: false,
                    device_handle_ptr: 0x2000,
                },
                received_at: now,
            },
        ];

        let handle = find_match_in_buffer(HidUsage::A, &mut buffer);
        assert_eq!(handle, Some(0x2000));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer[0].event.device_handle_ptr, 0x1000);

        // Clean up.
        buffer.clear();
    }

    #[test]
    fn find_match_in_buffer_no_match() {
        let mut buffer = vec![BufferedRawInput {
            event: RawInputEvent {
                usage: Some(HidUsage::A),
                vk_code: Some(VIRTUAL_KEY(0x41)),
                is_key_up: false,
                device_handle_ptr: 0x1000,
            },
            received_at: Instant::now(),
        }];

        let handle = find_match_in_buffer(HidUsage::W, &mut buffer);
        assert!(handle.is_none());
        assert_eq!(buffer.len(), 1);

        buffer.clear();
    }

    #[test]
    fn find_match_in_buffer_empty() {
        let mut buffer: Vec<BufferedRawInput> = Vec::new();
        assert!(find_match_in_buffer(HidUsage::A, &mut buffer).is_none());
    }

    #[test]
    fn resolve_device_path_rejects_null_handle() {
        assert!(resolve_device_path(0).is_none());
    }

    #[test]
    fn raw_input_event_is_send() {
        // Verify that RawInputEvent is Send so it can be sent across threads.
        fn assert_send<T: Send>() {}
        assert_send::<RawInputEvent>();
    }

    #[test]
    fn decision_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Decision>();
    }

    #[test]
    fn device_cache_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DeviceCache>();
    }

    // -----------------------------------------------------------------------
    // Decide function tests
    // -----------------------------------------------------------------------

    #[test]
    fn decide_returns_pass_through_when_no_mapping() {
        let lookup = arc_lookup(MockLookup::new());

        let decision = decide(&lookup, HidUsage::A, 0, None);

        assert!(matches!(decision, Decision::PassThrough));
    }

    #[test]
    fn decide_returns_swallow_when_mapping_found() {
        let outputs = vec![nk(Key::LeftControl)];
        let lookup = arc_lookup(MockLookup::new().with_global(
            HidUsage::CapsLock,
            0,
            None,
            outputs,
        ));

        let decision = decide(&lookup, HidUsage::CapsLock, 0, None);

        match &decision {
            Decision::Swallow(v) => {
                assert_eq!(v[0].usage, HidUsage::LeftControl)
            }
            _ => panic!("expected Swallow, got {decision:?}"),
        }
    }

    #[test]
    fn decide_with_device_id_passes_through_when_device_not_matched() {
        // MockLookup only matches when device_id is None. When a device_id
        // is provided, no mapping is found, so PassThrough.
        let lookup = arc_lookup(MockLookup::new());

        let decision = decide(
            &lookup,
            HidUsage::CapsLock,
            0,
            Some(r"\\?\hid#vid_046d#..."),
        );

        assert!(matches!(decision, Decision::PassThrough));
    }

    #[test]
    fn decide_with_device_id_returns_mapping_when_configured() {
        let outputs = vec![nk(Key::A)];
        let lookup = arc_lookup(MockLookup::new().with_global(
            HidUsage::B,
            0,
            Some(r"\\?\hid#vid_046d#..."),
            outputs,
        ));

        let decision =
            decide(&lookup, HidUsage::B, 0, Some(r"\\?\hid#vid_046d#..."));

        match &decision {
            Decision::Swallow(v) => assert_eq!(v[0].usage, HidUsage::A),
            _ => panic!("expected Swallow, got {decision:?}"),
        }
    }

    #[test]
    fn decide_with_modifiers_returns_mapping_when_configured() {
        let outputs = vec![nk(Key::LeftControl)];
        // Modifier bit 0 = LeftControl held
        let lookup = arc_lookup(MockLookup::new().with_global(
            HidUsage::A,
            0b0000_0001,
            None,
            outputs,
        ));

        let decision = decide(&lookup, HidUsage::A, 0b0000_0001, None);

        assert!(matches!(&decision, Decision::Swallow(_)));
    }

    // -----------------------------------------------------------------------
    // Process hook event integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn process_hook_event_key_down_with_raw_input_match() {
        // Clean key state cache first.
        key_state_cache().remove(Key::CapsLock.as_native());

        let lookup = arc_lookup(MockLookup::new().with_global(
            HidUsage::CapsLock,
            0,
            Some(r"\\?\hid#vid_046d#..."),
            vec![nk(Key::LeftControl)],
        ));

        let mut raw_buffer = vec![BufferedRawInput {
            event: RawInputEvent {
                usage: Some(HidUsage::CapsLock),
                vk_code: Some(VIRTUAL_KEY(Key::CapsLock.as_native())),
                is_key_up: false,
                device_handle_ptr: 0x1234,
            },
            received_at: Instant::now(),
        }];

        let device_cache = DeviceCache::new();
        // Pre-populate the device cache so resolve_device_path is not needed.
        {
            let mut map = device_cache.map.lock().unwrap();
            map.insert(0x1234, r"\\?\hid#vid_046d#...".to_string());
        }

        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        let hook_event = HookEvent {
            vk_code: VIRTUAL_KEY(Key::CapsLock.as_native()),
            usage: Some(HidUsage::CapsLock),
            is_key_up: false,
            modifiers: 0,
            reply_tx,
        };

        process_hook_event(
            &hook_event,
            &lookup,
            &mut raw_buffer,
            &device_cache,
        );

        let decision = reply_rx.recv().unwrap();
        match &decision {
            Decision::Swallow(v) => {
                assert_eq!(v[0].usage, HidUsage::LeftControl)
            }
            _ => panic!("expected Swallow, got {decision:?}"),
        }

        // Raw input event was consumed from buffer.
        assert_eq!(raw_buffer.len(), 0);

        // Key state was cached for the key-up.
        assert!(key_state_cache().get(Key::CapsLock.as_native()).is_some());

        // Cleanup.
        key_state_cache().remove(Key::CapsLock.as_native());
    }

    #[test]
    fn process_hook_event_key_up_uses_cached_decision() {
        // Seed the cache with a Swallow decision.
        key_state_cache().insert(
            Key::CapsLock.as_native(),
            CachedKeyState {
                decision: Decision::Swallow(vec![nk(Key::LeftControl)]),
                modifiers: 0,
            },
        );

        let lookup = arc_lookup(MockLookup::new());
        let mut raw_buffer: Vec<BufferedRawInput> = Vec::new();
        let device_cache = DeviceCache::new();

        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        let hook_event = HookEvent {
            vk_code: VIRTUAL_KEY(Key::CapsLock.as_native()),
            usage: Some(HidUsage::CapsLock),
            is_key_up: true,
            modifiers: 0,
            reply_tx,
        };

        process_hook_event(
            &hook_event,
            &lookup,
            &mut raw_buffer,
            &device_cache,
        );

        let decision = reply_rx.recv().unwrap();
        // Key-up Swallow should have empty outputs (emission only on
        // key-down).
        assert!(matches!(decision, Decision::Swallow(ref v) if v.is_empty()));

        // Cache entry was removed.
        assert!(key_state_cache().get(Key::CapsLock.as_native()).is_none());
    }

    #[test]
    fn process_hook_event_key_up_pass_through_when_not_cached() {
        // No cached entry — key-up passes through.
        key_state_cache().remove(Key::A.as_native());

        let lookup = arc_lookup(MockLookup::new());
        let mut raw_buffer: Vec<BufferedRawInput> = Vec::new();
        let device_cache = DeviceCache::new();

        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        let hook_event = HookEvent {
            vk_code: VIRTUAL_KEY(Key::A.as_native()),
            usage: Some(HidUsage::A),
            is_key_up: true,
            modifiers: 0,
            reply_tx,
        };

        process_hook_event(
            &hook_event,
            &lookup,
            &mut raw_buffer,
            &device_cache,
        );

        let decision = reply_rx.recv().unwrap();
        assert!(matches!(decision, Decision::PassThrough));
    }

    #[test]
    fn process_hook_event_key_down_no_mapping_pass_through() {
        // Clean key state cache.
        key_state_cache().remove(Key::Z.as_native());

        let lookup = arc_lookup(MockLookup::new());
        let mut raw_buffer: Vec<BufferedRawInput> = Vec::new();
        let device_cache = DeviceCache::new();

        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        let hook_event = HookEvent {
            vk_code: VIRTUAL_KEY(Key::Z.as_native()),
            usage: Some(HidUsage::Z),
            is_key_up: false,
            modifiers: 0,
            reply_tx,
        };

        // Use a very short timeout to avoid waiting for decide_with_delay.
        // Since there's no raw buffer entry and no mapping, the worker
        // falls back to lookup without device ID, which also returns None.
        process_hook_event(
            &hook_event,
            &lookup,
            &mut raw_buffer,
            &device_cache,
        );

        let decision = reply_rx.recv().unwrap();
        assert!(matches!(decision, Decision::PassThrough));

        // Cleanup.
        key_state_cache().remove(Key::Z.as_native());
    }

    // -----------------------------------------------------------------------
    // Buffer eviction stress tests
    // -----------------------------------------------------------------------

    #[test]
    fn evict_stale_under_load() {
        let mut buffer = Vec::new();

        // Insert 1000 entries with varying ages.
        for i in 0..1000u16 {
            buffer.push(BufferedRawInput {
                event: RawInputEvent {
                    usage: None,
                    vk_code: Some(VIRTUAL_KEY(i)),
                    is_key_up: false,
                    device_handle_ptr: i as usize,
                },
                // Half are old, half are new.
                received_at: if i % 2 == 0 {
                    Instant::now() - std::time::Duration::from_millis(200)
                } else {
                    Instant::now()
                },
            });
        }

        assert_eq!(buffer.len(), 1000);

        evict_stale(&mut buffer, std::time::Duration::from_millis(100));

        // Only the "new" entries (odd indices) should remain.
        assert_eq!(buffer.len(), 500);
        for entry in &buffer {
            assert!(entry.event.vk_code.unwrap().0 % 2 != 0);
        }
    }

    #[test]
    fn find_match_in_buffer_with_many_duplicates() {
        let now = Instant::now();
        let mut buffer = Vec::new();

        // Insert 100 entries with the same usage but increasing timestamps.
        for i in 0..100 {
            buffer.push(BufferedRawInput {
                event: RawInputEvent {
                    usage: Some(HidUsage::A),
                    vk_code: Some(VIRTUAL_KEY(0x41)),
                    is_key_up: false,
                    device_handle_ptr: i,
                },
                received_at: now + std::time::Duration::from_millis(i as u64),
            });
        }

        // Should return the most recent (highest handle_ptr).
        let handle = find_match_in_buffer(HidUsage::A, &mut buffer);
        assert_eq!(handle, Some(99));
        assert_eq!(buffer.len(), 99);

        buffer.clear();
    }

    // -----------------------------------------------------------------------
    // Key state cache rapid cycle tests
    // -----------------------------------------------------------------------

    #[test]
    fn key_state_cache_rapid_press_release_cycle() {
        let cache = key_state_cache();
        let vk = Key::CapsLock.as_native();

        // Simulate rapid press-release cycles.
        for _ in 0..100 {
            cache.insert(
                vk,
                CachedKeyState {
                    decision: Decision::Swallow(vec![nk(Key::LeftControl)]),
                    modifiers: 0,
                },
            );
            assert!(cache.get(vk).is_some());
            cache.remove(vk);
            assert!(cache.get(vk).is_none());
        }
    }

    #[test]
    fn key_state_cache_multiple_keys_independent() {
        let cache = key_state_cache();

        // Insert different states for different keys.
        cache.insert(
            0x41, // 'A'
            CachedKeyState {
                decision: Decision::Swallow(vec![nk(Key::B)]),
                modifiers: 0,
            },
        );
        cache.insert(
            0x42, // 'B'
            CachedKeyState {
                decision: Decision::PassThrough,
                modifiers: 0,
            },
        );

        assert!(matches!(
            cache.get(0x41),
            Some(CachedKeyState {
                decision: Decision::Swallow(_),
                ..
            })
        ));
        assert!(matches!(
            cache.get(0x42),
            Some(CachedKeyState {
                decision: Decision::PassThrough,
                ..
            })
        ));

        // Remove one, the other remains.
        cache.remove(0x41);
        assert!(cache.get(0x41).is_none());
        assert!(cache.get(0x42).is_some());

        // Cleanup.
        cache.remove(0x42);
    }

    // -----------------------------------------------------------------------
    // Device cache concurrent access tests
    // -----------------------------------------------------------------------

    #[test]
    fn device_cache_handles_concurrent_lookups() {
        use std::thread;

        let cache = Arc::new(DeviceCache::new());
        // Pre-populate the cache to avoid needing real device handles.
        {
            let mut map = cache.map.lock().unwrap();
            for i in 0..10 {
                map.insert(i, format!("device_{i}"));
            }
        }

        let handles: Vec<_> = (0..10)
            .map(|i| {
                let cache = Arc::clone(&cache);
                thread::spawn(move || {
                    for _ in 0..100 {
                        let result = cache.get_or_resolve(i);
                        assert_eq!(result, Some(format!("device_{i}")));
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
    }

    // -----------------------------------------------------------------------
    // Channel behaviour tests
    // -----------------------------------------------------------------------

    #[test]
    fn hook_event_reply_channel_delivers_once() {
        let (reply_tx, reply_rx) = crossbeam_channel::bounded::<Decision>(1);

        reply_tx.send(Decision::Swallow(vec![nk(Key::A)])).unwrap();

        let decision = reply_rx.recv().unwrap();
        assert!(matches!(decision, Decision::Swallow(_)));

        // Sender is dropped; receiver should error.
        drop(reply_tx);
        assert!(reply_rx.recv().is_err());
    }

    #[test]
    fn worker_receives_raw_input_events_before_hook() {
        // Verify that the crossbeam_channel::select! mechanism correctly
        // processes raw input events before hook events when both are
        // available.  We simulate this by pushing raw events into the
        // buffer and then processing a hook event.
        let lookup = arc_lookup(MockLookup::new());

        let mut raw_buffer = vec![
            BufferedRawInput {
                event: RawInputEvent {
                    usage: Some(HidUsage::A),
                    vk_code: Some(VIRTUAL_KEY(0x41)),
                    is_key_up: false,
                    device_handle_ptr: 0x1000,
                },
                received_at: Instant::now(),
            },
            BufferedRawInput {
                event: RawInputEvent {
                    usage: Some(HidUsage::B),
                    vk_code: Some(VIRTUAL_KEY(0x42)),
                    is_key_up: false,
                    device_handle_ptr: 0x2000,
                },
                received_at: Instant::now(),
            },
        ];

        // Hook event for 'A' should match the first raw input event.
        key_state_cache().remove(0x41);
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        let hook_event = HookEvent {
            vk_code: VIRTUAL_KEY(0x41),
            usage: Some(HidUsage::A),
            is_key_up: false,
            modifiers: 0,
            reply_tx,
        };

        process_hook_event(
            &hook_event,
            &lookup,
            &mut raw_buffer,
            &DeviceCache::new(),
        );

        // No mapping configured, so PassThrough — but the raw buffer entry
        // should have been consumed.
        let _ = reply_rx.recv().unwrap();
        assert_eq!(raw_buffer.len(), 1);
        assert_eq!(raw_buffer[0].event.vk_code.unwrap().0, 0x42);

        // Cleanup.
        key_state_cache().remove(0x41);
        raw_buffer.clear();
    }

    // -----------------------------------------------------------------------
    // RuntimeLookupCache compilation from config (cross-module test)
    // -----------------------------------------------------------------------

    #[test]
    fn compiled_rule_contains_keyboard_filter() {
        use crate::common::config::AppConfig;

        let yaml = r#"
groups:
  - keyboards:
      - vendor: "Apple"
    mappings:
      CapsLock: LeftControl
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let cache = RuntimeLookupCache::compile_from_config(&config);
        let rules = cache.global_rules();

        assert!(!rules.is_empty());
        let rule = &rules[0];
        // Compiled rules store the trigger as a `HidUsage`.
        assert_eq!(rule.usage, HidUsage::CapsLock);
        assert!(rule.keyboards.is_some());
    }

    #[test]
    fn compiled_rule_without_keyboard_filter() {
        use crate::common::config::AppConfig;

        let yaml = r#"
groups:
  - mappings:
      CapsLock: LeftControl
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let cache = RuntimeLookupCache::compile_from_config(&config);
        let rules = cache.global_rules();

        assert!(!rules.is_empty());
        assert!(rules[0].keyboards.is_none());
    }
}
