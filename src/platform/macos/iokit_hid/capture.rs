// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Event processing for seized HID keyboards.
//!
//! Every key is re-emitted through the virtual keyboard: mapped keys as their
//! mapped output, unmapped keys forwarded unchanged.  A seized keyboard is
//! invisible to the OS, so anything not re-emitted would be lost; forwarding
//! unmapped keys keeps the seized keyboards usable for normal typing.

use std::{collections::HashSet, ffi::c_void, sync::Arc};

use super::{
    device::{HidDevice, HidDeviceManager, HidQueueHandle},
    ffi::{
        CFRelease, HID_USAGE_PAGE_CONSUMER, HID_USAGE_PAGE_KEYBOARD,
        IOHIDElementGetUsage, IOHIDElementGetUsagePage, IOHIDQueue,
        IOHIDQueueCopyNextValue, IOHIDValue, IOHIDValueGetElement,
        IOHIDValueGetIntegerValue, IoKitError,
    },
};
use crate::{
    common::hid_usage::{HidUsage, PAGE_CONSUMER},
    platform::macos::karabiner_client::KarabinerClient,
};

// ---------------------------------------------------------------------------
// HID value callback context
// ---------------------------------------------------------------------------

/// Context passed to the queue value-available callback.
pub struct HidQueueContext {
    // Shared lookup for remapping rules.
    pub lookup:
        std::sync::Arc<parking_lot::RwLock<dyn crate::daemon::state::Lookup>>,
    // Shared client to the Karabiner DriverKit virtual HID keyboard.
    pub conn: Arc<KarabinerClient>,
    // Bitmask tracking which modifier keys are physically pressed.
    pub modifier_state: u8,
    // Set of currently pressed keycodes for deduplication.
    pub pressed_keys: std::collections::HashSet<u16>,
    // Device location ID string for keyboard filtering.
    pub device_id: String,
    // Usage ids of non-modifier keys that were forwarded (unmapped) and are
    // still held.  The virtual keyboard report is a state snapshot, so every
    // forwarded report must include all of these.
    pub forwarded_keys: HashSet<u16>,
    // Full usage codes of keys whose key-down was mapped, so their key-up is
    // swallowed rather than forwarded.
    pub mapped_keys: HashSet<u32>,
    // Bitmask of modifier keys that were forwarded (unmapped) and are still
    // held.  Mapped modifiers are excluded so their self-contained output
    // taps do not leak into forwarded reports.
    pub forwarded_modifiers: u8,
    // Bitmask of modifier keys that were part of a fired trigger and have
    // already been released on the virtual keyboard.  Their physical release
    // is swallowed so it is not forwarded a second time.
    pub consumed_modifiers: u8,
}

/// FFI callback invoked by IOHIDQueue when values are available.
///
/// Matches the C `IOHIDCallback` signature: `(void *context, IOReturn
/// result, void *sender)`, where `sender` is the queue.  The values are not
/// passed in; they must be drained from the queue with
/// `IOHIDQueueCopyNextValue` (non-blocking) until it returns NULL.  Each
/// returned value is a retained copy and must be released after processing.
pub(super) unsafe extern "C" fn hid_queue_value_callback(
    user_info: *mut c_void,
    _result: i32,
    queue: *mut IOHIDQueue,
) {
    if user_info.is_null() || queue.is_null() {
        return;
    }

    let context = unsafe { &mut *(user_info as *mut HidQueueContext) };

    // Drain the queue; each value is a retained copy that must be released.
    while let Some(value_ref) = unsafe { IOHIDQueueCopyNextValue(queue) } {
        process_hid_value(context, value_ref);
        unsafe { CFRelease(value_ref as *const _) };
    }
}

/// Process a single `IOHIDValue` from the seized device's queue.
///
/// Extracts usage page, usage code, and value from the element that produced
/// the value, then dispatches to [`process_key_event`].  Only keyboard and
/// consumer page values are processed; all other values are ignored.
fn process_hid_value(
    context: &mut HidQueueContext,
    value_ref: *mut IOHIDValue,
) {
    // Get the element that produced this value.
    let element = unsafe { IOHIDValueGetElement(value_ref) };
    if element.is_null() {
        return;
    }

    // Extract usage page and usage code.
    let usage_page = unsafe { IOHIDElementGetUsagePage(element) };
    let usage = unsafe { IOHIDElementGetUsage(element) } as u16;

    // Skip non-keyboard/consumer events.
    if usage_page != HID_USAGE_PAGE_KEYBOARD
        && usage_page != HID_USAGE_PAGE_CONSUMER
    {
        return;
    }

    // Get the value (0 = up, non-zero = down).
    let raw_value = unsafe { IOHIDValueGetIntegerValue(value_ref) };
    let is_down = raw_value != 0;

    // Construct HidUsage from raw HID page/id.  Use this for all
    // modifier tracking, deduplication, and key identification.
    let Some(hid_usage) =
        HidUsage::from_code(usage_page << 16 | (usage as u32))
    else {
        // Unknown usage — let it pass through.
        return;
    };

    process_key_event(context, hid_usage, is_down);
}

/// Process a single key event (down or up) identified by its `HidUsage`.
///
/// This is the core of the capture logic, invoked by the IOKit queue
/// callback for every seized keyboard (physical keyboards and the e2e
/// injection keyboard alike).  It performs deduplication, modifier tracking,
/// rule lookup, and emission/forwarding.
fn process_key_event(
    context: &mut HidQueueContext,
    hid_usage: HidUsage,
    is_down: bool,
) {
    // Track pressed keys for deduplication.  Use the raw HID usage id
    // (page-specific, unambiguous).
    let key_id = hid_usage.id();

    if is_down {
        // Key-down.  Ignore auto-repeat (the key is already tracked).
        if !context.pressed_keys.insert(key_id) {
            return;
        }

        // Get the device ID for keyboard filtering.
        let device_id = Some(context.device_id.as_str());

        // Track modifier state using HidUsage directly.  The lookup uses
        // the state captured before this key's own bit is set, so a bare
        // modifier trigger does not match itself.
        let lookup_modifiers = context.modifier_state;
        if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage) {
            context.modifier_state |= 1 << bit;
        }

        // Perform the lookup.  Compiled rules store the trigger as a
        // `HidUsage`, so the lookup is keyed by the full page-specific
        // usage.
        let guard = context.lookup.read();
        let active_outputs = guard
            .for_app(
                &guard.active_app(),
                hid_usage,
                lookup_modifiers,
                device_id,
            )
            .or_else(|| guard.global(hid_usage, lookup_modifiers, device_id))
            .map(|v| v.to_vec());
        drop(guard);

        if let Some(outputs) = active_outputs {
            // The trigger's modifiers were forwarded when pressed.  Release
            // them now so the output is emitted as a clean tap: holding them
            // would produce an unintended control sequence (e.g. the rule
            // Ctrl+Semicolon -> C would emit Ctrl+C, i.e. SIGINT).  Mark them
            // consumed so their physical release is swallowed below.
            let consumed = lookup_modifiers & context.forwarded_modifiers;
            if consumed != 0 {
                context.forwarded_modifiers &= !consumed;
                context.consumed_modifiers |= consumed;
                post_forwarded_state(
                    &context.conn,
                    &context.forwarded_keys,
                    context.forwarded_modifiers,
                );
            }

            // Mapped: emit the mapped outputs via the virtual HID keyboard.
            // Remember the key was mapped so its release is swallowed rather
            // than forwarded.
            for native_key in &outputs {
                emit_hid_report(
                    &context.conn,
                    native_key,
                    &context.forwarded_keys,
                    context.forwarded_modifiers,
                );
            }
            context.mapped_keys.insert(hid_usage.code());
        } else {
            // Unmapped: forward the key through the virtual keyboard so it
            // reaches the OS unchanged.
            forward_key_down(context, hid_usage);
        }
    } else {
        // Key-up.  Ignore releases for keys that were never tracked as down.
        if !context.pressed_keys.remove(&key_id) {
            return;
        }

        // Clear the modifier bit so subsequent forwarded reports carry the
        // correct modifier state.
        if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage) {
            context.modifier_state &= !(1 << bit);
        }

        // A consumed modifier (part of a fired trigger) was already released
        // on the virtual keyboard when the trigger fired; swallow its release.
        if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage)
            && context.consumed_modifiers & (1 << bit) != 0
        {
            context.consumed_modifiers &= !(1 << bit);
            return;
        }

        // A mapped key's release is swallowed; a forwarded key's release
        // is forwarded.
        if !context.mapped_keys.remove(&hid_usage.code()) {
            forward_key_up(context, hid_usage);
        }
    }
}

/// Drain a queue of `IOHIDValueRef` and invoke `f` for each recognized
/// keyboard/consumer key event, passing the combined HID usage code
/// (`(page << 16) | id`) and whether the key is down.
///
/// Values are pulled from the queue with `IOHIDQueueCopyNextValue`
/// (non-blocking) until it returns NULL; each returned value is a retained
/// copy and is released after processing.  Non-keyboard/consumer values are
/// skipped.  Used by the e2e monitor's logging callback to extract key
/// events from the seized virtual keyboard.
///
/// # Safety
///
/// `queue` must be a valid, open `IOHIDQueue` that outlives the call, and
/// the callback `f` must not panic (a panic would leak the retained value
/// currently being processed).
pub unsafe fn for_each_hid_value(
    queue: *mut IOHIDQueue,
    mut f: impl FnMut(u32, bool),
) {
    if queue.is_null() {
        return;
    }

    while let Some(value_ref) = unsafe { IOHIDQueueCopyNextValue(queue) } {
        let element = unsafe { IOHIDValueGetElement(value_ref) };

        if !element.is_null() {
            let usage_page = unsafe { IOHIDElementGetUsagePage(element) };

            if usage_page == HID_USAGE_PAGE_KEYBOARD
                || usage_page == HID_USAGE_PAGE_CONSUMER
            {
                let usage = unsafe { IOHIDElementGetUsage(element) };
                let raw_value =
                    unsafe { IOHIDValueGetIntegerValue(value_ref) };

                f((usage_page << 16) | usage, raw_value != 0);
            }
        }

        // Each value is a retained copy; release it after processing.
        unsafe { CFRelease(value_ref as *const _) };
    }
}

/// Emit a single `NativeKey` through the Karabiner virtual keyboard.
///
/// Emits the output as a sequence of state-snapshot reports so that every key
/// transition is its own report: each output modifier down (ascending bit
/// order), the base key down, the base key up, then each output modifier up
/// (descending bit order).  Posting one transition per report makes the
/// captured event order deterministic — a single report carrying both a
/// modifier and the base key would let IOKit deliver the two values in an
/// unspecified order.  Because each report is a full state snapshot, it also
/// carries the currently-held forwarded keys and modifier byte so that
/// emitting a mapped output does not clear them (already-held keys are
/// repeats the monitor suppresses).
/// Dispatches on the output usage's page:
/// - Keyboard page (0x07): a 67-byte `keyboard_input` report (32 × 16-bit
///   usages).
/// - Consumer page (0x0C): a `consumer_input` report.
fn emit_hid_report(
    conn: &Arc<KarabinerClient>,
    native_key: &crate::daemon::mapping_cache::NativeKey,
    forwarded_keys: &HashSet<u16>,
    forwarded_modifiers: u8,
) {
    use crate::common::hid_usage::PAGE_KEYBOARD;

    if native_key.usage.page() == PAGE_KEYBOARD {
        let base_usage = native_key.usage.id();
        let output_modifiers = native_key.modifiers;

        // State snapshots with and without the base key.  Sorted for a
        // deterministic report layout (slot order is irrelevant to the
        // virtual keyboard's state tracking, but determinism aids debugging).
        let mut usages_with_base: Vec<u16> =
            forwarded_keys.iter().copied().collect();
        usages_with_base.push(base_usage);
        usages_with_base.sort_unstable();

        let mut usages_without_base: Vec<u16> =
            forwarded_keys.iter().copied().collect();
        usages_without_base.sort_unstable();

        // Press each output modifier, one at a time in ascending bit order,
        // so the captured event order is deterministic.
        let mut modifiers = forwarded_modifiers;
        for bit in 0..8 {
            if (output_modifiers >> bit) & 1 == 1 {
                modifiers |= 1 << bit;
                let _ =
                    conn.send_keyboard_report(modifiers, &usages_without_base);
            }
        }

        // Press the base key with all output modifiers held, then release it.
        let _ = conn.send_keyboard_report(modifiers, &usages_with_base);
        let _ = conn.send_keyboard_report(modifiers, &usages_without_base);

        // Release each output modifier, one at a time in descending bit order.
        for bit in (0..8).rev() {
            if (output_modifiers >> bit) & 1 == 1 {
                modifiers &= !(1 << bit);
                let _ =
                    conn.send_keyboard_report(modifiers, &usages_without_base);
            }
        }
    } else {
        // Consumer page: post the usage, then an all-clear report to release.
        let _ = conn.send_consumer_report(native_key.usage.id());
        let _ = conn.send_consumer_release();
    }
}

/// Forward an unmapped key-down through the virtual keyboard.
///
/// Keyboard-page keys are added to the held set and the full state snapshot
/// is posted; consumer-page keys are pressed directly.
fn forward_key_down(context: &mut HidQueueContext, hid_usage: HidUsage) {
    if hid_usage.page() == PAGE_CONSUMER {
        let _ = context.conn.send_consumer_report(hid_usage.id());
        return;
    }

    if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage) {
        // Modifier: tracked in the modifier byte, not a usage slot.
        context.forwarded_modifiers |= 1 << bit;
    } else {
        context.forwarded_keys.insert(hid_usage.id());
    }

    post_forwarded_state(
        &context.conn,
        &context.forwarded_keys,
        context.forwarded_modifiers,
    );
}

/// Forward an unmapped key-up through the virtual keyboard.
fn forward_key_up(context: &mut HidQueueContext, hid_usage: HidUsage) {
    if hid_usage.page() == PAGE_CONSUMER {
        let _ = context.conn.send_consumer_release();
        return;
    }

    if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(hid_usage) {
        context.forwarded_modifiers &= !(1 << bit);
    } else {
        context.forwarded_keys.remove(&hid_usage.id());
    }

    post_forwarded_state(
        &context.conn,
        &context.forwarded_keys,
        context.forwarded_modifiers,
    );
}

/// Post the current forwarded keyboard state as a `keyboard_input` report.
///
/// The report is a state snapshot: it carries every held forwarded key and the
/// forwarded modifier byte.  The virtual keyboard emits a down for each newly
/// present usage and an up for each usage that is no longer present.
fn post_forwarded_state(
    conn: &Arc<KarabinerClient>,
    forwarded_keys: &HashSet<u16>,
    modifiers: u8,
) {
    let mut usages: Vec<u16> = forwarded_keys.iter().copied().collect();
    // Sort for a deterministic report layout (slot order is irrelevant to the
    // virtual keyboard's state tracking, but determinism aids debugging).
    usages.sort_unstable();
    let _ = conn.send_keyboard_report(modifiers, &usages);
}

// ---------------------------------------------------------------------------
// Public: start IOHID device-seizure based mapping
// ---------------------------------------------------------------------------

/// Handle that keeps all seized devices and queues alive.  Drop to release.
pub struct SeizureHandle {
    _manager: HidDeviceManager,
    _devices: Vec<HidDevice>,
    _queue_handles: Vec<HidQueueHandle<HidQueueContext>>,
}

// ---------------------------------------------------------------------------
// Entry point — IOKit device seizure with queue-based capture
// ---------------------------------------------------------------------------

/// Whether a device passes the global keyboard filter.
///
/// Returns `true` when the filter is unset or empty (all keyboards pass), or
/// when the device matches at least one specifier.
fn device_matches_filter(
    device: &HidDevice,
    filter: Option<&[crate::common::keyboard::KeyboardSpecifier]>,
) -> bool {
    let Some(specs) = filter else {
        return true;
    };
    if specs.is_empty() {
        return true;
    }
    let info = device.keyboard_info();
    specs.iter().any(|spec| spec.matches(&info))
}

/// Start keyboard input capture via IOKit device seizure.
///
/// This follows the Karabiner Elements approach:
/// 1. Discover keyboards via IOHIDManager (matching only).
/// 2. Open each device with `kIOHIDOptionsTypeSeizeDevice`.
/// 3. Create an IOHIDQueue for each device and register a value callback.
/// 4. Run the CFRunLoop to receive events.
///
/// Every key is re-emitted through the shared `KarabinerClient` (the Karabiner
/// DriverKit virtual keyboard): mapped keys as their mapped output, unmapped
/// keys forwarded unchanged.
///
/// Requires root privileges and Input Monitoring permission.
pub fn start_iohid_seizure_mapping(
    lookup: std::sync::Arc<
        parking_lot::RwLock<dyn crate::daemon::state::Lookup>,
    >,
    conn: Arc<KarabinerClient>,
    keyboard_filter: Option<&[crate::common::keyboard::KeyboardSpecifier]>,
) -> Result<SeizureHandle, IoKitError> {
    // Discover physical keyboards.
    let manager = HidDeviceManager::new_keyboard_matcher()?;
    let discovered = manager.scan_devices();

    if discovered.is_empty() {
        return Err(IoKitError::IoReturn(
            0,
            "No keyboard devices found via IOHIDManager".into(),
        ));
    }

    println!(
        "IOKit HID: discovered {} keyboard device(s)",
        discovered.len(),
    );

    // Apply the global keyboard filter: only seize keyboards the user wants
    // to remap.  Non-matching keyboards are left alone so they keep working
    // normally (a seized keyboard is invisible to the OS).
    let devices: Vec<_> = discovered
        .into_iter()
        .filter(|device| device_matches_filter(device, keyboard_filter))
        .collect();

    if devices.is_empty() {
        eprintln!(
            "IOKit HID: no keyboards match the global filter; nothing to \
             seize"
        );
    }

    // Open and seize each device, creating queues.
    let mut queue_handles = Vec::new();

    for device in &devices {
        let device_id = device.location_id_string();
        println!("IOKit HID: seizing device at location {}", device_id,);

        // Seize the device.
        device.open(true).map_err(|e| {
            eprintln!(
                "IOKit HID: failed to seize device {}: {}",
                device_id, e
            );
            e
        })?;

        // Create a queue and register the callback.
        let queue = device.create_queue()?;

        // Build the context for this device's callback.
        let ctx = HidQueueContext {
            lookup: lookup.clone(),
            conn: conn.clone(),
            modifier_state: 0,
            pressed_keys: std::collections::HashSet::new(),
            device_id,
            forwarded_keys: HashSet::new(),
            mapped_keys: HashSet::new(),
            forwarded_modifiers: 0,
            consumed_modifiers: 0,
        };

        let handle = queue.register_value_callback(ctx);

        // Schedule and open the queue.
        queue.schedule_with_runloop();
        queue.open()?;

        println!(
            "IOKit HID: queue active for device {}",
            device.location_id_string()
        );

        queue_handles.push(handle);
    }

    // Schedule the manager for hotplug.
    manager.schedule_with_runloop();

    Ok(SeizureHandle {
        _manager: manager,
        _devices: devices,
        _queue_handles: queue_handles,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_bit_from_hid_usage_left_control() {
        let usage = HidUsage::LeftControl;
        assert_eq!(HidUsage::hid_usage_to_modifier_bit(usage), Some(0));
    }

    #[test]
    fn modifier_bit_from_hid_usage_non_modifier() {
        let usage = HidUsage::A;
        assert_eq!(HidUsage::hid_usage_to_modifier_bit(usage), None);
    }

    #[test]
    fn modifier_bit_from_hid_usage_consumer_page() {
        let usage = HidUsage::PlayPause;
        assert_eq!(HidUsage::hid_usage_to_modifier_bit(usage), None);
    }
}
