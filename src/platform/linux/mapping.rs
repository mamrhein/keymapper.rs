// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    os::unix::io::{AsRawFd, RawFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use evdev::{
    AttributeSet, Device, EventType, InputEvent, KeyCode, MiscCode,
    uinput::VirtualDevice,
};
use libc::{c_int, epoll_event, epoll_wait as libc_epoll_wait};
use parking_lot::{Mutex, RwLock};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag::register,
};
use udev::{Enumerator, MonitorBuilder};

use super::{
    hid_translate::{hid_usage_to_keycode, keycode_to_hid_usage},
    keyboard::discover_and_open_keyboards,
};
use crate::{
    common::{
        hid_usage::HidUsage,
        keyboard::{
            KeyboardInfo, KeyboardSpecifier, filter_keyboards_by_specifiers,
        },
        modifier::ModifierRole,
    },
    daemon::{mapping_cache::NativeKey, state::Lookup},
};

/// Name of the daemon's own uinput output device.
///
/// Exposed so the e2e monitor (Linux direct-capture mode) can locate and
/// grab the device; the daemon itself never grabs it (see
/// [`handle_device_add`]).
pub const VIRTUAL_KEYBOARD_NAME: &str = "CrossPlatform_Virtual_Keyboard";

// ---------------------------------------------------------------------------
// Raw epoll FFI for cross-thread operations
// ---------------------------------------------------------------------------
//
// The nix `Epoll` type owns the epoll fd but is not `Sync`, preventing
// safe sharing across threads.  Because epoll_ctl(2) and epoll_wait(2) are
// safe to call concurrently on the same epoll fd, we use thin libc wrappers
// for all epoll operations.  This avoids the nix `Epoll` type entirely and
// gives us full control over cross-thread access.

unsafe extern "C" {
    fn epoll_create1(flags: c_int) -> c_int;
    fn epoll_ctl(
        epfd: c_int,
        op: c_int,
        fd: c_int,
        event: *mut epoll_event,
    ) -> c_int;
}

/// Raw epoll control operation constants.
const EPOLL_CTL_ADD: c_int = 1;
const EPOLL_CTL_DEL: c_int = 2;

/// EPOLLIN | EPOLLET flag combination.
const EPOLL_IN_ET: u32 = 0x001 | (1 << 31);

/// Create a new epoll instance.  Returns the file descriptor on success.
fn epoll_create() -> Result<c_int, std::io::Error> {
    let fd = unsafe { epoll_create1(0) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(fd)
    }
}

/// Register a file descriptor with the epoll instance.
fn epoll_add(epfd: c_int, fd: c_int, data: u64) -> Result<(), std::io::Error> {
    let mut event = epoll_event {
        events: EPOLL_IN_ET,
        u64: data,
    };
    let ret = unsafe { epoll_ctl(epfd, EPOLL_CTL_ADD, fd, &mut event) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Remove a file descriptor from the epoll instance.
fn epoll_del(epfd: c_int, fd: c_int) -> Result<(), std::io::Error> {
    let ret =
        unsafe { epoll_ctl(epfd, EPOLL_CTL_DEL, fd, std::ptr::null_mut()) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Wait for events on the epoll instance.  Blocks indefinitely.
fn epoll_wait_raw(
    epfd: c_int,
    events: &mut [epoll_event],
) -> Result<c_int, std::io::Error> {
    let ret = unsafe {
        libc_epoll_wait(
            epfd,
            events.as_mut_ptr(),
            events.len() as c_int,
            -1, // Block indefinitely.
        )
    };
    if ret >= 0 {
        Ok(ret)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Manages the lifecycle of an epoll file descriptor.
///
/// Wraps a raw fd in a type-safe wrapper that closes the fd on drop.
struct EpollFd(c_int);

impl EpollFd {
    fn new() -> Result<Self, std::io::Error> {
        epoll_create().map(EpollFd)
    }

    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Drop for EpollFd {
    fn drop(&mut self) {
        let _ = unsafe { libc::close(self.0) };
    }
}

// ---------------------------------------------------------------------------
// Per-device state
// ---------------------------------------------------------------------------

/// A single managed keyboard device, tracking its own modifier state.
struct ManagedDevice {
    device: Device,
    /// Device node path (e.g. `/dev/input/event3`), used for rule lookup.
    path: String,
    /// Bitmask of currently active modifiers for this device only.
    modifiers: u8,
    /// Bitmask of forwarded (unmapped) modifier keys that are still held.
    /// Mapped modifiers are excluded so their self-contained output taps
    /// do not leak into later state.
    forwarded_modifiers: u8,
    /// Bitmask of modifier keys that were part of a fired trigger and have
    /// already been released on the virtual keyboard.  Their physical
    /// release is swallowed so it is not forwarded a second time.
    consumed_modifiers: u8,
    /// Last received `MSC_SCAN` value, consumed by the next `EV_KEY`
    /// event.  The kernel emits the scan code before the key event of the
    /// same press; key-ups and repeats carry no scan code, so those fall
    /// back to the `EV_KEY` reverse lookup.
    pending_scan: Option<u32>,
}

// ---------------------------------------------------------------------------
// Modifier handling
// ---------------------------------------------------------------------------

/// Map a modifier bit position to the evdev `KEY_*` code for emission.
///
/// The bit resolves to a `ModifierRole` (the canonical layout lives in
/// `common::modifier`); the resulting modifier usage is looked up in the
/// shared `hid_translate` table like any other key.
fn modifier_bit_to_keycode(bit: u8) -> Option<u16> {
    let role = ModifierRole::try_from_bit(bit)?;
    let usage = HidUsage::keyboard(role.hid_id())?;
    hid_usage_to_keycode(usage)
}

/// Spacing between sub-events of a chord emission.
///
/// Windowing backends and e2e monitor windows sample keyboard state once
/// per frame (typically 16-30 ms).  A tap that fits entirely between two
/// samples is invisible to them, so each sub-event is held long enough to
/// guarantee at least one sample inside the press window.
const EMIT_SPACING: Duration = Duration::from_millis(20);

/// Emit a complete key event (press+release) through the virtual device.
///
/// Handles chord emission: modifiers are pressed, the base key is toggled,
/// then modifiers are released in reverse order. On failure, any keys that
/// were pressed are released to prevent stuck state.
fn emit_key_event(
    device: &mut VirtualDevice,
    native_key: &NativeKey,
) -> Result<(), Box<dyn std::error::Error>> {
    // Raw evdev event type codes.
    const EV_KEY: u16 = 1;
    const EV_SYN: u16 = 0;
    const SYN_REPORT: u16 = 0;

    // Resolve the output's base key to an evdev `KEY_*` code via the
    // static HID translation table.
    let Some(base_code) = hid_usage_to_keycode(native_key.usage) else {
        return Err(format!(
            "no evdev key code for HID usage {:?}",
            native_key.usage
        )
        .into());
    };

    // Track all pressed codes so they can be released on failure.
    let mut pressed: Vec<u16> = Vec::new();

    // Helper to emit a single event with synchronization.
    let emit = |dev: &mut VirtualDevice,
                code: u16,
                val: i32|
     -> Result<(), Box<dyn std::error::Error>> {
        dev.emit(&[
            InputEvent::new(EV_KEY, code, val),
            InputEvent::new(EV_SYN, SYN_REPORT, 0),
        ])?;
        Ok(())
    };

    // Helper to release any keys that were successfully pressed.
    let cleanup = |dev: &mut VirtualDevice, codes: &[u16]| {
        for code in codes.iter().rev() {
            let _ = emit(dev, *code, 0);
            thread::sleep(EMIT_SPACING);
        }
    };

    // Press modifiers.
    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1
            && let Some(code) = modifier_bit_to_keycode(bit)
        {
            emit(device, code, 1)?;
            pressed.push(code);
            thread::sleep(EMIT_SPACING);
        }
    }

    // Press and release the base key.
    emit(device, base_code, 1)?;
    thread::sleep(Duration::from_millis(1));
    emit(device, base_code, 0)?;
    thread::sleep(Duration::from_millis(1));

    // Release modifiers in reverse order.
    cleanup(device, &pressed);

    Ok(())
}

// ---------------------------------------------------------------------------
// Per-device event processing
// ---------------------------------------------------------------------------

/// Process all pending events for a single managed device.
///
/// Uses the device's own modifier state and path for rule lookup, ensuring
/// that modifier state on one keyboard does not affect another.
fn process_device_events(
    managed: &mut ManagedDevice,
    virtual_device: &mut VirtualDevice,
    lookup: &Arc<RwLock<dyn Lookup>>,
) {
    // Drain all pending events from this non-blocking device.
    let events = match managed.device.fetch_events() {
        Ok(events) => events,
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            return;
        }
        Err(e) => {
            eprintln!(
                "Linux: error reading events from {}: {}",
                managed.path, e
            );
            return;
        }
    };

    for event in events {
        // MSC_SCAN events carry the raw HID usage as
        // `(page << 16) | id`, and the kernel emits them before the
        // EV_KEY event of the same key press.  Buffer the scan code for
        // the next key event.
        if event.event_type() == EventType::MISC
            && event.code() == MiscCode::MSC_SCAN.0
        {
            managed.pending_scan = Some(event.value() as u32);
            continue;
        }

        if event.event_type() != EventType::KEY {
            continue;
        }

        let code = event.code();
        let value = event.value();

        // Derive the HID identity of this key.  MSC_SCAN is preferred;
        // the EV_KEY reverse lookup covers key-ups, auto-repeats, and
        // devices that do not emit MSC_SCAN.
        let usage = managed
            .pending_scan
            .take()
            .and_then(HidUsage::from_code)
            .or_else(|| keycode_to_hid_usage(code));

        let Some(usage) = usage else {
            // Unknown key with no resolvable HID identity: forward it
            // unchanged.
            forward_key_event(virtual_device, code, value);
            continue;
        };

        // Capture the modifier state to use for rule matching.  For modifier
        // keys this is the pre-update snapshot so that bare-modifier triggers
        // (e.g. "LeftControl: A") match correctly against the concurrent
        // modifier set.
        let lookup_modifiers = managed.modifiers;

        if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage) {
            if value == 1 {
                managed.modifiers |= 1 << bit;
            } else if value == 0 {
                managed.modifiers &= !(1 << bit);
            }
        }

        let device_path = &managed.path;

        // Compiled rules store the trigger as a `HidUsage`, so the
        // lookup is keyed by the full page-specific usage.
        let guard = lookup.read();
        let active_outputs = guard
            .for_active_app(usage, lookup_modifiers, Some(device_path))
            .or_else(|| {
                guard.global(usage, lookup_modifiers, Some(device_path))
            })
            .map(|v| v.to_vec());
        drop(guard);

        if let Some(outputs) = active_outputs {
            // Emit mapped outputs and swallow the original event.  This
            // applies to modifier keys as well: if a bare modifier
            // (e.g. LeftControl alone) is mapped, its outputs are emitted
            // and the original modifier press is NOT forwarded to the
            // virtual device, preventing double emission.
            if value == 1 {
                // The trigger's modifiers were forwarded when pressed.
                // Release them now so the output is emitted as a clean tap;
                // mark them consumed so their physical release is swallowed
                // below rather than forwarded a second time.
                let consumed = lookup_modifiers & managed.forwarded_modifiers;
                if consumed != 0 {
                    managed.forwarded_modifiers &= !consumed;
                    managed.consumed_modifiers |= consumed;
                    release_consumed_modifiers(virtual_device, consumed);
                }

                for native_key in &outputs {
                    if let Err(e) = emit_key_event(virtual_device, native_key)
                    {
                        eprintln!("emit error: {}", e);
                    }
                }
            }
            continue;
        }

        // A modifier key-up that did not fire a trigger: a consumed modifier
        // (already released when its trigger fired) and a mapped
        // bare-modifier trigger's release are both swallowed, while a
        // forwarded modifier's release is forwarded and untracked.
        if value == 0
            && let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage)
        {
            let mask = 1u8 << bit;
            if managed.consumed_modifiers & mask != 0 {
                managed.consumed_modifiers &= !mask;
                continue;
            }
            if managed.forwarded_modifiers & mask == 0 {
                // The press was mapped (a bare-modifier trigger) and never
                // forwarded, so its release is swallowed.
                continue;
            }
            managed.forwarded_modifiers &= !mask;
        }

        // Track a forwarded (unmapped) modifier press so a later fired
        // trigger can release it cleanly.
        if value == 1
            && let Some(bit) = HidUsage::hid_usage_to_modifier_bit(usage)
        {
            managed.forwarded_modifiers |= 1 << bit;
        }

        // Forward the event to the virtual device.
        forward_key_event(virtual_device, code, value);
    }
}

/// Forward a raw evdev key event to the virtual device.
///
/// Repeat events (value == 2) are emitted as a press+release pair to
/// avoid key-stick on the virtual device.
fn forward_key_event(device: &mut VirtualDevice, code: u16, value: i32) {
    // Raw evdev event type codes.
    const EV_KEY: u16 = 1;
    const EV_SYN: u16 = 0;
    const SYN_REPORT: u16 = 0;

    let events = if value == 2 {
        // Repeat event: emit as press+release to avoid key-stick.
        vec![
            InputEvent::new(EV_KEY, code, 1),
            InputEvent::new(EV_KEY, code, 0),
            InputEvent::new(EV_SYN, SYN_REPORT, 0),
        ]
    } else {
        vec![
            InputEvent::new(EV_KEY, code, value),
            InputEvent::new(EV_SYN, SYN_REPORT, 0),
        ]
    };

    if let Err(e) = device.emit(&events) {
        eprintln!("emit error: {e}");
    }
}

/// Release a set of consumed trigger modifiers on the virtual device.
///
/// Emits a key-up for each set bit in *consumed* (ascending bit order) so
/// the fired trigger's modifiers are dropped before the mapped output is
/// emitted.  Without this the output would ride on the still-held modifier
/// and produce an unintended control sequence (e.g. the rule
/// `Ctrl+Semicolon -> C` would emit Ctrl+C, i.e. SIGINT).
fn release_consumed_modifiers(device: &mut VirtualDevice, consumed: u8) {
    // Raw evdev event type codes.
    const EV_KEY: u16 = 1;
    const EV_SYN: u16 = 0;
    const SYN_REPORT: u16 = 0;

    for bit in 0..8 {
        if consumed & (1 << bit) != 0
            && let Some(code) = modifier_bit_to_keycode(bit)
        {
            let _ = device.emit(&[
                InputEvent::new(EV_KEY, code, 0),
                InputEvent::new(EV_SYN, SYN_REPORT, 0),
            ]);
            thread::sleep(EMIT_SPACING);
        }
    }
}

// ---------------------------------------------------------------------------
// Hot-plug monitor (udev-based, filter-aware)
// ---------------------------------------------------------------------------

/// Spawn a background thread that listens for keyboard device add/remove
/// events via udev and dynamically updates the managed device set.
///
/// New devices are only grabbed if they match the global keyboard filter.
/// Removed devices are ungrabbed and removed from the epoll set.
///
/// **Limitation:** changes to the global `keyboards:` filter at runtime do
/// not affect the grab list. The user must restart the daemon for
/// filter changes to take effect on hot-plugged devices.
fn start_hotplug_monitor(
    managed_devices: Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
    global_filter: Option<Vec<KeyboardSpecifier>>,
) {
    use udev::EventType;

    thread::Builder::new()
        .name("keymapper-hotplug".into())
        .spawn(move || {
            // Set up the udev monitor.
            let socket = match MonitorBuilder::new() {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("warning: failed to create udev monitor: {e}");
                    return;
                }
            };

            let socket = match socket.match_subsystem("input") {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("warning: failed to match input subsystem: {e}");
                    return;
                }
            };

            let socket = match socket.listen() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("warning: failed to start udev monitor: {e}");
                    return;
                }
            };

            println!("Hot-plug monitor started.");

            // Resync: the startup udev snapshot in `start_mapping` and this
            // monitor's `listen()` call are not atomic.  A keyboard added in
            // between (e.g. a test injector created moments before daemon
            // start, or a device whose udev tagging finished after the
            // snapshot) never emits a fresh "add" event to this monitor and
            // would otherwise never be grabbed.  Rescan and adopt any
            // missing keyboards to close that window.
            resync_devices(&managed_devices, epoll_fd, &global_filter);

            for event in socket.iter() {
                let udev_device = event.device();

                // Filter for keyboards manually, since the netlink monitor
                // doesn't support property-based filtering.
                let is_keyboard = udev_device
                    .property_value("ID_INPUT_KEYBOARD")
                    .map(|s| s.to_string_lossy() == "1")
                    .unwrap_or(false);
                if !is_keyboard {
                    continue;
                }

                match event.event_type() {
                    EventType::Add => {
                        handle_device_add(
                            &udev_device,
                            &managed_devices,
                            epoll_fd,
                            &global_filter,
                        );
                    }
                    EventType::Remove => {
                        handle_device_remove(
                            &udev_device,
                            &managed_devices,
                            epoll_fd,
                        );
                    }
                    _ => {}
                }
            }
        })
        .expect("failed to spawn hot-plug monitor thread");
}

/// Rescan udev for keyboards that are not yet managed and adopt them.
///
/// Called once after the hot-plug monitor starts listening, to cover the
/// race window between the startup snapshot and the monitor's `listen()`
/// call.  Reuses [`handle_device_add`], which skips devices that are
/// already managed.
fn resync_devices(
    managed_devices: &Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
    global_filter: &Option<Vec<KeyboardSpecifier>>,
) {
    let Ok(mut enumerator) = Enumerator::new() else {
        eprintln!("warning: resync: failed to create udev enumerator");
        return;
    };

    if enumerator.match_subsystem("input").is_err()
        || enumerator.match_property("ID_INPUT_KEYBOARD", "1").is_err()
    {
        eprintln!("warning: resync: failed to configure udev enumerator");
        return;
    }

    let Ok(devices) = enumerator.scan_devices() else {
        eprintln!("warning: resync: failed to scan udev devices");
        return;
    };

    for udev_device in devices {
        handle_device_add(
            &udev_device,
            managed_devices,
            epoll_fd,
            global_filter,
        );
    }
}

/// Handle a udev "add" event for a keyboard device.
///
/// Opens the device, checks the global filter, grabs it, and registers it
/// with epoll and the managed device list.
fn handle_device_add(
    udev_device: &udev::Device,
    managed_devices: &Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
    global_filter: &Option<Vec<KeyboardSpecifier>>,
) {
    // Build keyboard info and open the evdev device.
    let Some((kb, mut device)) =
        super::keyboard::build_keyboard_from_udev(udev_device)
    else {
        return;
    };

    // Skip the daemon's own virtual output device, which udev also tags as
    // a keyboard.  Grabbing it would feed the daemon's emitted events back
    // into its input loop, causing them to be re-emitted indefinitely.
    if kb.name == VIRTUAL_KEYBOARD_NAME {
        return;
    }

    // Check if it matches the global filter.
    let filtered = filter_keyboards_by_specifiers(
        std::slice::from_ref(&kb),
        global_filter.as_deref(),
    );
    if filtered.is_empty() {
        println!(
            "Hot-plug: ignoring {} (does not match global filter)",
            kb.name
        );
        return;
    }

    // Skip if this device is already managed.
    {
        let devices = managed_devices.lock();
        if devices.iter().any(|m| m.path == kb.device) {
            return;
        }
    }

    // Grab and configure the device.
    if let Err(e) = device.grab() {
        eprintln!("warning: failed to grab {}: {e}", kb.device);
        return;
    }

    if let Err(e) = device.set_nonblocking(true) {
        eprintln!("warning: failed to set non-blocking on {}: {e}", kb.device);
        return;
    }

    let fd = device.as_raw_fd();
    let managed = ManagedDevice {
        device,
        path: kb.device.clone(),
        modifiers: 0,
        forwarded_modifiers: 0,
        consumed_modifiers: 0,
        pending_scan: None,
    };

    // Register with managed devices.
    {
        let mut devices = managed_devices.lock();
        devices.push(managed);
    }

    // Register with epoll.
    if let Err(e) = epoll_add(epoll_fd, fd, fd as u64) {
        eprintln!("warning: failed to add {} to epoll: {e}", kb.device);
        // Rollback: remove from managed devices since epoll registration
        // failed.
        let mut devices = managed_devices.lock();
        if let Some(idx) = devices.iter().position(|m| m.path == kb.device) {
            devices.remove(idx);
        }
        return;
    }

    println!("Hot-plug: grabbed {} ({})", kb.device, kb.name);
}

/// Handle a udev "remove" event for a keyboard device.
///
/// Removes the device from epoll and the managed device list.
fn handle_device_remove(
    udev_device: &udev::Device,
    managed_devices: &Arc<Mutex<Vec<ManagedDevice>>>,
    epoll_fd: RawFd,
) {
    // Get the device path to identify the managed device.
    let dev_path = match udev_device.devnode() {
        Some(d) => d.to_string_lossy().into_owned(),
        None => {
            // Cannot identify the device without a devnode.
            eprintln!("warning: remove event without devnode, skipping");
            return;
        }
    };

    // Remove from managed devices and capture the fd for epoll cleanup.
    let fd = {
        let mut devices = managed_devices.lock();
        let idx = match devices.iter().position(|m| m.path == dev_path) {
            Some(i) => i,
            None => {
                // Not managed — nothing to do.
                return;
            }
        };

        let fd = devices[idx].device.as_raw_fd();
        devices.remove(idx); // Drops ManagedDevice, closing the fd.
        fd
    };

    // Delete from epoll.  The fd is now closed, but the kernel handles
    // this gracefully.  If the kernel already cleaned it up, this may
    // fail — log and ignore.
    if let Err(e) = epoll_del(epoll_fd as c_int, fd) {
        eprintln!("warning: failed to remove {dev_path} from epoll: {e}");
    }

    println!("Hot-plug: removed {dev_path}");
}

// ---------------------------------------------------------------------------
// evdev event loop (epoll-based, multi-device)
// ---------------------------------------------------------------------------

/// `ready_signal` is invoked once the daemon can process events; it is
/// injected by the caller so this module stays free of test-specific side
/// effects.
pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    keyboard_filter: Option<Vec<KeyboardSpecifier>>,
    ready_signal: Option<Box<dyn FnOnce() + Send>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Discover and open keyboards for capture.  Degrade gracefully: with no
    // keyboards the daemon starts with an empty managed set, and the hot-plug
    // monitor picks devices up as they appear.
    let opened = discover_and_open_keyboards().unwrap_or_default();

    // Select the devices matching the keyboard filter, then filter the opened
    // pairs down to that grab set.
    let infos: Vec<KeyboardInfo> =
        opened.iter().map(|(info, _)| info.clone()).collect();
    let to_grab: Vec<KeyboardInfo> =
        filter_keyboards_by_specifiers(&infos, keyboard_filter.as_deref());
    let grab_paths: std::collections::HashSet<&str> =
        to_grab.iter().map(|kb| kb.device.as_str()).collect();
    let opened_to_grab: Vec<(KeyboardInfo, Device)> = opened
        .into_iter()
        .filter(|(info, _)| grab_paths.contains(info.device.as_str()))
        .collect();

    if opened_to_grab.is_empty() {
        println!("No keyboards to grab. Waiting for events...");
    }

    // Grab and register all opened keyboards.
    let mut managed_devices: Vec<ManagedDevice> = Vec::new();
    for (kb, mut device) in opened_to_grab {
        device.grab()?;
        device.set_nonblocking(true)?;

        println!("Grabbed keyboard: {} ({})", kb.device, kb.name);
        managed_devices.push(ManagedDevice {
            device,
            path: kb.device,
            modifiers: 0,
            forwarded_modifiers: 0,
            consumed_modifiers: 0,
            pending_scan: None,
        });
    }

    // KEY_CNT is the total number of key codes defined by the kernel
    // (linux/input.h: #define KEY_CNT (KEY_MAX + 1), where KEY_MAX = 0x2fd).
    const KEY_CNT: u16 = 0x2fe;
    let all_keys: AttributeSet<KeyCode> =
        (0..KEY_CNT).map(KeyCode::new).collect();
    let mut virtual_device = VirtualDevice::builder()?
        .name(VIRTUAL_KEYBOARD_NAME)
        .with_keys(&all_keys)?
        .build()?;

    thread::sleep(Duration::from_millis(200));
    println!("Linux virtual keyboard ready.");

    let shutdown = Arc::new(AtomicBool::new(false));
    register(SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    register(SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");

    // Set up epoll for multiplexing across all managed devices.  `EpollFd`
    // owns the epoll fd and closes it on drop.
    let epoll_fd = EpollFd::new().map_err(|e| {
        eprintln!("Linux: failed to create epoll instance: {e}");
        e
    })?;

    for managed in &managed_devices {
        let fd = managed.device.as_raw_fd();
        epoll_add(epoll_fd.as_raw_fd(), fd, fd as u64)?;
    }

    // Share the managed devices vector with the hot-plug monitor.  The main
    // event loop locks it briefly while processing each device's events.
    let managed_devices = Arc::new(Mutex::new(managed_devices));

    // Start hot-plug monitor for dynamic device add/remove.
    start_hotplug_monitor(
        Arc::clone(&managed_devices),
        epoll_fd.as_raw_fd(),
        keyboard_filter,
    );

    // All devices are grabbed, the virtual output device is created, and the
    // hot-plug monitor is running, so the daemon can now process events.
    if let Some(signal) = ready_signal {
        signal();
    }

    let mut events = vec![epoll_event { events: 0, u64: 0 }; 64];

    while !shutdown.load(Ordering::Acquire) {
        match epoll_wait_raw(epoll_fd.as_raw_fd(), &mut events) {
            Ok(n) => {
                for event in &events[..n as usize] {
                    let fd = event.u64 as RawFd;

                    // Find the managed device for this file descriptor and
                    // process its events.  The lock is held during processing
                    // to prevent the hot-plug thread from modifying the vec
                    // concurrently.  Hot-plug operations are rare, so the
                    // contention is negligible.
                    let mut devices = managed_devices.lock();
                    if let Some(managed) =
                        devices.iter_mut().find(|m| m.device.as_raw_fd() == fd)
                    {
                        process_device_events(
                            managed,
                            &mut virtual_device,
                            &lookup,
                        );
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                // Signal interruption — normal, just loop again.
                continue;
            }
            Err(e) => {
                eprintln!("Linux: epoll wait error: {e}");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    println!("Shutdown signal received. Cleaning up...");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::keyboard::{KeyboardInfo, KeyboardSpecifier};

    // -----------------------------------------------------------------------
    // Multi-device epoll integration test
    // -----------------------------------------------------------------------
    //
    // Verifies that epoll correctly multiplexes events from multiple file
    // descriptors.  We use pipe(2) fds as stand-ins for evdev devices, since
    // real devices require root uinput access in test environments.

    /// RAII wrapper that closes a raw fd on drop.
    struct FdGuard(c_int);

    impl FdGuard {
        fn new(fd: c_int) -> Self {
            FdGuard(fd)
        }
    }

    impl Drop for FdGuard {
        fn drop(&mut self) {
            unsafe { libc::close(self.0) };
        }
    }

    /// Create a pipe and return (read_fd, write_fd) wrapped in guards.
    fn make_pipe() -> (FdGuard, FdGuard) {
        let mut fds: [c_int; 2] = [0; 2];
        let ret = unsafe { libc::pipe2(fds.as_mut_ptr(), 0) };
        assert_eq!(ret, 0, "pipe2 failed");
        (FdGuard::new(fds[0]), FdGuard::new(fds[1]))
    }

    #[test]
    fn epoll_multiplexes_multiple_devices() {
        // Create two pipe pairs to simulate two independent devices.
        let (rd_a, wr_a) = make_pipe();
        let (rd_b, wr_b) = make_pipe();

        // Set up epoll and register both read ends.
        let epfd = epoll_create().expect("epoll_create");
        let fd_a = rd_a.0;
        let fd_b = rd_b.0;
        epoll_add(epfd, fd_a, fd_a as u64).expect("epoll_add A");
        epoll_add(epfd, fd_b, fd_b as u64).expect("epoll_add B");

        // Write a byte to pipe A only.
        let buf_a: u8 = 42;
        let ret =
            unsafe { libc::write(wr_a.0, &buf_a as *const _ as *const _, 1) };
        assert!(ret >= 0, "write to pipe A failed");

        // epoll_wait should return one event for pipe A.
        let mut events = vec![epoll_event { events: 0, u64: 0 }; 8];
        let n = epoll_wait_raw(epfd, &mut events).expect("epoll_wait");
        assert_eq!(n, 1, "expected exactly one epoll event");
        assert_eq!(
            events[0].u64 as RawFd, fd_a,
            "event should come from pipe A"
        );

        // Drain the byte from pipe A.
        let mut buf = [0u8; 1];
        let _ = unsafe { libc::read(fd_a, buf.as_mut_ptr() as *mut _, 1) };

        // Write to pipe B and verify it's the one that triggers.
        let buf_b: u8 = 99;
        let ret =
            unsafe { libc::write(wr_b.0, &buf_b as *const _ as *const _, 1) };
        assert!(ret >= 0, "write to pipe B failed");

        let mut events = vec![epoll_event { events: 0, u64: 0 }; 8];
        let n = epoll_wait_raw(epfd, &mut events).expect("epoll_wait");
        assert_eq!(n, 1, "expected exactly one epoll event");
        assert_eq!(
            events[0].u64 as RawFd, fd_b,
            "event should come from pipe B"
        );

        // Clean up: remove from epoll, then let guards close fds.
        let _ = epoll_del(epfd, fd_a);
        let _ = epoll_del(epfd, fd_b);
    }

    // -----------------------------------------------------------------------
    // Per-device modifier isolation tests
    // -----------------------------------------------------------------------
    //
    // Verifies that modifier state is tracked independently per device.
    // Ctrl pressed on device A must not affect the modifier bitmask of
    // device B.

    #[test]
    fn modifier_bit_to_keycode_maps_all_modifiers() {
        // All eight modifier bits resolve to the corresponding evdev
        // codes; bit 8 is out of range.
        assert_eq!(modifier_bit_to_keycode(0), Some(29)); // KEY_LEFTCTRL
        assert_eq!(modifier_bit_to_keycode(1), Some(42)); // KEY_LEFTSHIFT
        assert_eq!(modifier_bit_to_keycode(2), Some(56)); // KEY_LEFTALT
        assert_eq!(modifier_bit_to_keycode(3), Some(125)); // KEY_LEFTMETA
        assert_eq!(modifier_bit_to_keycode(4), Some(97)); // KEY_RIGHTCTRL
        assert_eq!(modifier_bit_to_keycode(5), Some(54)); // KEY_RIGHTSHIFT
        assert_eq!(modifier_bit_to_keycode(6), Some(100)); // KEY_RIGHTALT
        assert_eq!(modifier_bit_to_keycode(7), Some(126)); // KEY_RIGHTMETA
        assert_eq!(modifier_bit_to_keycode(8), None);
    }

    #[test]
    fn modifier_bit_matches_hid_usage_table() {
        // The bit->keycode path must agree with the shared HID usage
        // table for all eight modifier usages.
        for usage in [
            HidUsage::LeftControl,
            HidUsage::RightControl,
            HidUsage::LeftShift,
            HidUsage::RightShift,
            HidUsage::LeftAlt,
            HidUsage::RightAlt,
            HidUsage::LeftCommand,
            HidUsage::RightCommand,
        ] {
            let bit = HidUsage::hid_usage_to_modifier_bit(usage)
                .expect("modifier usage");
            assert_eq!(
                modifier_bit_to_keycode(bit),
                hid_usage_to_keycode(usage),
                "modifier round-trip failed for {usage:?}"
            );
        }
    }

    #[test]
    fn modifier_state_is_isolated_per_device() {
        // Simulate two independent devices by tracking their own modifier
        // bitmasks, mirroring the logic in `process_device_events`.
        let mut mods_a: u8 = 0;
        let mut mods_b: u8 = 0;

        // Device A: press LeftControl (bit 0).
        let bit = HidUsage::hid_usage_to_modifier_bit(HidUsage::LeftControl)
            .unwrap();
        mods_a |= 1 << bit;
        assert_eq!(mods_a, 0b0000_0001);
        assert_eq!(mods_b, 0); // Device B unaffected.

        // Device A: press LeftShift (bit 1).
        let bit =
            HidUsage::hid_usage_to_modifier_bit(HidUsage::LeftShift).unwrap();
        mods_a |= 1 << bit;
        assert_eq!(mods_a, 0b0000_0011);
        assert_eq!(mods_b, 0); // Device B unaffected.

        // Device B: press RightAlt (bit 6).
        let bit =
            HidUsage::hid_usage_to_modifier_bit(HidUsage::RightAlt).unwrap();
        mods_b |= 1 << bit;
        assert_eq!(mods_a, 0b0000_0011); // Device A unaffected.
        assert_eq!(mods_b, 0b0100_0000);

        // Device A: release LeftControl.
        let bit = HidUsage::hid_usage_to_modifier_bit(HidUsage::LeftControl)
            .unwrap();
        mods_a &= !(1 << bit);
        assert_eq!(mods_a, 0b0000_0010);
        assert_eq!(mods_b, 0b0100_0000);

        // Device B: release RightAlt, press LeftCommand (bit 3).
        let bit =
            HidUsage::hid_usage_to_modifier_bit(HidUsage::RightAlt).unwrap();
        mods_b &= !(1 << bit);
        let bit = HidUsage::hid_usage_to_modifier_bit(HidUsage::LeftCommand)
            .unwrap();
        mods_b |= 1 << bit;
        assert_eq!(mods_a, 0b0000_0010);
        assert_eq!(mods_b, 0b0000_1000);
    }

    // -----------------------------------------------------------------------
    // Filter-aware hot-plug tests
    // -----------------------------------------------------------------------
    //
    // Verifies that the keyboard filter used by `handle_device_add` correctly
    // allows matching devices and blocks non-matching ones.  The hot-plug
    // handler uses `filter_keyboards_by_specifiers` to decide whether to
    // grab a newly discovered device.

    fn build_keyboard(
        name: &str,
        vendor: &str,
        model: &str,
        device: &str,
        port: Option<&str>,
    ) -> KeyboardInfo {
        KeyboardInfo::new(
            name.to_string(),
            vendor.to_string(),
            model.to_string(),
            device.to_string(),
            port.map(|s| s.to_string()),
        )
    }

    #[test]
    fn hotplug_filter_allows_matching_device() {
        // A filter that matches by vendor.
        let specs = vec![KeyboardSpecifier {
            name: None,
            vendor: Some("Logitech".to_string()),
            model: None,
            port: None,
        }];

        let kb = build_keyboard(
            "Logitech K800",
            "Logitech",
            "K800",
            "/dev/input/event5",
            Some("USB"),
        );

        let filtered = filter_keyboards_by_specifiers(
            std::slice::from_ref(&kb),
            Some(&specs),
        );
        assert_eq!(filtered.len(), 1, "matching device should be grabbed");
        assert_eq!(filtered[0].name, "Logitech K800");
    }

    #[test]
    fn hotplug_filter_blocks_non_matching_device() {
        // A filter that only matches "Logitech" vendor.
        let specs = vec![KeyboardSpecifier {
            name: None,
            vendor: Some("Logitech".to_string()),
            model: None,
            port: None,
        }];

        // A different vendor — should NOT be grabbed.
        let kb = build_keyboard(
            "Apple Magic Keyboard",
            "Apple",
            "Magic Keyboard",
            "/dev/input/event6",
            Some("Bluetooth"),
        );

        let filtered = filter_keyboards_by_specifiers(
            std::slice::from_ref(&kb),
            Some(&specs),
        );
        assert!(
            filtered.is_empty(),
            "non-matching device should NOT be grabbed"
        );
    }

    #[test]
    fn hotplug_no_filter_grabs_all_devices() {
        // When no global filter is set, all discovered devices are grabbed.
        let kb = build_keyboard(
            "Some Keyboard",
            "Generic",
            "Model X",
            "/dev/input/event7",
            None,
        );

        let filtered =
            filter_keyboards_by_specifiers(std::slice::from_ref(&kb), None);
        assert_eq!(filtered.len(), 1, "no filter should grab all devices");
    }

    #[test]
    fn hotplug_empty_filter_grabs_all_devices() {
        // An empty filter list is equivalent to no filter.
        let specs: Vec<KeyboardSpecifier> = vec![];

        let kb = build_keyboard(
            "Some Keyboard",
            "Generic",
            "Model X",
            "/dev/input/event8",
            None,
        );

        let filtered = filter_keyboards_by_specifiers(
            std::slice::from_ref(&kb),
            Some(&specs),
        );
        assert_eq!(filtered.len(), 1, "empty filter should grab all devices");
    }

    #[test]
    fn hotplug_filter_matches_by_name() {
        // Name matching is exact (case-insensitive), not substring.
        let specs = vec![KeyboardSpecifier {
            name: Some("Logitech K800".to_string()),
            vendor: None,
            model: None,
            port: None,
        }];

        let kb = build_keyboard(
            "Logitech K800",
            "Logitech",
            "K800",
            "/dev/input/event9",
            Some("USB"),
        );

        let filtered = filter_keyboards_by_specifiers(
            std::slice::from_ref(&kb),
            Some(&specs),
        );
        assert_eq!(filtered.len(), 1, "name filter should match");
    }

    #[test]
    fn hotplug_filter_matches_by_port() {
        let specs = vec![KeyboardSpecifier {
            name: None,
            vendor: None,
            model: None,
            port: Some("Bluetooth".to_string()),
        }];

        let kb_bluetooth = build_keyboard(
            "BT Keyboard",
            "Vendor",
            "Model",
            "/dev/input/event10",
            Some("Bluetooth"),
        );

        let kb_usb = build_keyboard(
            "USB Keyboard",
            "Vendor",
            "Model",
            "/dev/input/event11",
            Some("USB"),
        );

        let filtered_bt = filter_keyboards_by_specifiers(
            std::slice::from_ref(&kb_bluetooth),
            Some(&specs),
        );
        assert_eq!(filtered_bt.len(), 1, "Bluetooth device should match");

        let filtered_usb = filter_keyboards_by_specifiers(
            std::slice::from_ref(&kb_usb),
            Some(&specs),
        );
        assert!(filtered_usb.is_empty(), "USB device should NOT match");
    }
}
