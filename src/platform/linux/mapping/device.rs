// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Per-managed-device state and input event processing.
//!
//! [`ManagedDevice`] wraps a single grabbed keyboard's evdev handle and its
//! [`MappingEngine`], which owns the modifier state and key-fate tracking that
//! is kept independently for each physical device.
//! [`process_device_events`] drains the device's pending input, resolves each
//! key's HID identity, and asks the engine for a decision, returning the
//! resulting output actions ([`EmitAction`]); [`emit_actions`] then executes
//! those actions against the virtual output device.  The split lets the event
//! loop decide under the short managed-device lock and run the paced sub-event
//! emissions (see [`EMIT_SPACING`]) outside it, so a mapped burst never blocks
//! the hot-plug thread.

use std::{
    os::unix::io::{AsRawFd, RawFd},
    thread,
    time::{Duration, Instant},
};

use evdev::{Device, EventType, InputEvent, MiscCode, uinput::VirtualDevice};
use log::{debug, error, trace, warn};

use crate::{
    common::{hid_usage::HidUsage, modifier::ModifierRole},
    daemon::{
        engine::{Decision, MappingEngine, fmt_native_key, output_held_mask},
        mapping_cache::NativeKey,
    },
    platform::linux::hid_translate::{
        hid_usage_to_keycode, keycode_to_hid_usage,
    },
};

// ---------------------------------------------------------------------------
// Per-device state
// ---------------------------------------------------------------------------

/// A single managed keyboard device, tracking its own modifier state.
pub(super) struct ManagedDevice {
    pub(super) device: Device,
    /// Device node path (e.g. `/dev/input/event3`), used for rule lookup.
    pub(super) path: String,
    /// The mapping engine: owns this device's modifier state and key-fate
    /// tracking (swallowed / forwarded / consumed).
    pub(super) engine: MappingEngine<u16>,
    /// Last received `MSC_SCAN` value, consumed by the next `EV_KEY`
    /// event.  The kernel emits the scan code before the key event of the
    /// same press; key-ups and repeats carry no scan code, so those fall
    /// back to the `EV_KEY` reverse lookup.
    pub(super) pending_scan: Option<u32>,
    /// Set when a hot-plug-adopted device still has held-modifier key-downs
    /// queued for re-emission (see `pending_held_modifiers`) but the
    /// hot-plug thread cannot reach the virtual device.  The event loop
    /// runs `sync_initial_state` once on the device's first pass, then
    /// clears this.  Devices grabbed at startup are synced inline in
    /// `start_mapping` and start `false`.
    pub(super) pending_initial_state: bool,
    /// Key codes of the modifiers held when the device was grabbed, queued
    /// for their key-down re-emission on the virtual device.  The engine is
    /// noted for every held key at grab time ([`capture_held_keys`]); only
    /// the emission is deferred until the virtual device exists, and
    /// [`sync_initial_state`] performs it.
    pub(super) pending_held_modifiers: Vec<u16>,
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

/// A single output operation decided for an input event: produced under the
/// managed-device lock by [`process_device_events`] / [`plan_initial_state`]
/// and executed later by [`emit_actions`] once the lock is released.
///
/// Deferring emission this way keeps the pacing sleeps ([`EMIT_SPACING`]) out
/// of the critical section: a mapped key with several
/// outputs — or a rapid burst — no longer holds the lock long enough to starve
/// the hot-plug thread or the other devices' events.
pub(super) enum EmitAction {
    /// Forward a raw key event unchanged: an unmapped press/release, the
    /// auto-repeat of an unmapped key, or the re-emitted key-down of a
    /// modifier held at grab time.
    Forward { code: u16, value: i32 },
    /// Release the fired trigger's consumed modifier bits before its output,
    /// so the output is a clean tap.
    ReleaseConsumed { consumed: u8 },
    /// Emit a self-contained mapped tap (modifiers, base, releases).
    Tap { native_key: NativeKey },
    /// Hold down a mapped modifier-key output on the virtual device.
    Hold { native_key: NativeKey },
}

/// Process all pending events for a single managed device.
///
/// Uses the device's own modifier state and path for rule lookup, ensuring
/// that modifier state on one keyboard does not affect another.
///
/// Runs entirely under the managed-device lock: it reads the device, decides
/// every event through the engine, and returns the ordered [`EmitAction`]s
/// without touching the virtual device or sleeping.  The caller drops the lock
/// and hands the result to [`emit_actions`].
pub(super) fn process_device_events(
    managed: &mut ManagedDevice,
) -> Vec<EmitAction> {
    // Drain all pending events from this non-blocking device.
    let events = match managed.device.fetch_events() {
        Ok(events) => events,
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            return Vec::new();
        }
        Err(e) => {
            error!("Linux: error reading events from {}: {}", managed.path, e);
            return Vec::new();
        }
    };
    let mut actions = Vec::new();

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
            // unchanged.  Such a key never reaches the engine's decide
            // path, so a key held at grab time is tracked stale by key code
            // only: its auto-repeat tail is dropped (re-emitting it would
            // inject a second press), and its release — forwarded, as
            // always — clears the mark.
            if value == 2 {
                if managed.engine.has_stale_key(code) {
                    continue;
                }
            } else if value == 0 {
                managed.engine.clear_stale_key(code);
            }
            actions.push(EmitAction::Forward { code, value });
            continue;
        };

        // The value distinguishes the two `EV_KEY` events a single tap emits
        // (a key-down and a key-up), which share the same code and usage.
        // Without it the down and up are indistinguishable in the log and
        // look like the key was received twice.
        let action = match value {
            0 => "up",
            1 => "down",
            _ => "repeat",
        };
        // Both events share the same line, but the down (and repeats, which
        // are downs for the engine) is the informative one and stays at
        // `debug`; the key-up is less useful and is logged at `trace` so it
        // stays out of the default debug output while remaining available.
        if value == 0 {
            trace!("recv {} {code} {action} -> {usage}", managed.path);
        } else {
            debug!("recv {} {code} {action} -> {usage}", managed.path);
        }

        // The engine decides the event's fate from its own bookkeeping
        // (pressed/swallowed keys, forwarded/consumed modifier masks, held
        // output modifiers); this layer only executes the decision.  A repeat
        // (value 2) is a key-down for this purpose: the engine deduplicates
        // it against its pressed set.
        match managed.engine.decide(
            code,
            usage,
            value != 0,
            Some(&managed.path),
            true,
        ) {
            // Unmapped (or the repeat of an unmapped key): forward the raw
            // event to the virtual device.
            Decision::Pass => {
                if value == 0 {
                    trace!("pass {} {code} {action} -> {usage}", managed.path);
                } else {
                    debug!("pass {} {code} {action} -> {usage}", managed.path);
                }
                actions.push(EmitAction::Forward { code, value });
            }
            // Mapped: release the trigger's modifiers first (clean tap),
            // then emit the outputs.  An output whose base is itself a
            // modifier key is held down on the virtual keyboard (not tapped)
            // so the remapped modifier stays active for subsequent key
            // presses; the matching release is emitted when the physical
            // key-up arrives.
            Decision::Emit { release, outputs } => {
                if release != 0 {
                    actions.push(EmitAction::ReleaseConsumed {
                        consumed: release,
                    });
                }
                for native_key in &outputs {
                    debug!("emit {}", fmt_native_key(native_key));
                    if output_held_mask(native_key).is_some() {
                        actions.push(EmitAction::Hold {
                            native_key: native_key.clone(),
                        });
                    } else {
                        actions.push(EmitAction::Tap {
                            native_key: native_key.clone(),
                        });
                    }
                }
            }
            // A mapped key-up: swallow the event and, for a remapped modifier
            // key, release the output bits that have been held since the
            // key-down.
            Decision::Swallow { release } => {
                trace!("swal {} {code} {action} -> {usage}", managed.path);
                if release != 0 {
                    actions.push(EmitAction::ReleaseConsumed {
                        consumed: release,
                    });
                }
            }
            // A consumed modifier release: the virtual device already released
            // the modifier when the trigger fired, so swallow the physical
            // release.
            Decision::ConsumedRelease => {
                trace!("swal {} {code} {action} -> {usage}", managed.path);
            }
        }
    }

    actions
}

/// Execute a batch of output actions produced by [`process_device_events`] or
/// [`plan_initial_state`] against the virtual output device.
///
/// Runs **outside** the managed-device lock: the sub-event pacing sleeps
/// ([`EMIT_SPACING`]) happen here rather than in the critical section, so a
/// mapped burst no longer stalls the hot-plug thread or the other devices'
/// events.  The actions are replayed in the exact order they were
/// decided, so the emitted key sequence is unchanged.
pub(super) fn emit_actions(
    device: &mut VirtualDevice,
    actions: &[EmitAction],
) {
    for action in actions {
        match action {
            EmitAction::Forward { code, value } => {
                forward_key_event(device, *code, *value);
            }
            EmitAction::ReleaseConsumed { consumed } => {
                release_consumed_modifiers(device, *consumed);
            }
            EmitAction::Tap { native_key } => {
                if let Err(e) = emit_key_event(device, native_key) {
                    error!("Emit error: {e}");
                }
            }
            EmitAction::Hold { native_key } => {
                if let Err(e) = hold_modifier_output(device, native_key) {
                    error!("Emit error: {e}");
                }
            }
        }
    }
}

/// Emit the key-downs of a modifier-key output, keeping them held on the
/// virtual keyboard.
///
/// Unlike [`emit_key_event`] (which emits a self-contained tap), the keys
/// stay down until the physical key-up — which is what makes a remapped
/// modifier usable for the key presses that follow it.  On failure, any
/// keys that were pressed are released to prevent stuck state.
fn hold_modifier_output(
    device: &mut VirtualDevice,
    native_key: &NativeKey,
) -> Result<(), Box<dyn std::error::Error>> {
    // Raw evdev event type codes.
    const EV_KEY: u16 = 1;
    const EV_SYN: u16 = 0;
    const SYN_REPORT: u16 = 0;

    // The base modifier is pressed after the output's other modifier bits,
    // and its own bit is not emitted twice.
    let base_bit = HidUsage::hid_usage_to_modifier_bit(native_key.usage);
    let mut codes: Vec<u16> = Vec::new();
    for bit in 0..8 {
        if (native_key.modifiers >> bit) & 1 == 1
            && base_bit != Some(bit)
            && let Some(code) = modifier_bit_to_keycode(bit)
        {
            codes.push(code);
        }
    }
    if let Some(bit) = base_bit
        && let Some(code) = modifier_bit_to_keycode(bit)
    {
        codes.push(code);
    }

    // Track all pressed codes so they can be released on failure.
    let mut pressed: Vec<u16> = Vec::new();
    for code in codes {
        if let Err(e) = device.emit(&[
            InputEvent::new(EV_KEY, code, 1),
            InputEvent::new(EV_SYN, SYN_REPORT, 0),
        ]) {
            for pressed_code in pressed.iter().rev() {
                let _ = device.emit(&[
                    InputEvent::new(EV_KEY, *pressed_code, 0),
                    InputEvent::new(EV_SYN, SYN_REPORT, 0),
                ]);
                thread::sleep(EMIT_SPACING);
            }
            return Err(e.into());
        }
        pressed.push(code);
        thread::sleep(EMIT_SPACING);
    }

    Ok(())
}

/// Forward a raw evdev key event to the virtual device.
///
/// A repeat (value == 2) of a **modifier** is a no-op: its down is already
/// out, and emitting a press+release pair would release it mid-hold,
/// corrupting the modifier state for every key that follows it.  A repeat of
/// any other key is emitted as a press+release pair so each tick stays
/// visible to windowing backends that sample keyboard state once per frame.
fn forward_key_event(device: &mut VirtualDevice, code: u16, value: i32) {
    // Raw evdev event type codes.
    const EV_KEY: u16 = 1;
    const EV_SYN: u16 = 0;
    const SYN_REPORT: u16 = 0;

    if value == 2 {
        let is_modifier = keycode_to_hid_usage(code)
            .and_then(HidUsage::hid_usage_to_modifier_bit)
            .is_some();
        if is_modifier {
            return;
        }
        // Non-modifier repeat: emit as press+release to avoid key-stick.
        let events = [
            InputEvent::new(EV_KEY, code, 1),
            InputEvent::new(EV_KEY, code, 0),
            InputEvent::new(EV_SYN, SYN_REPORT, 0),
        ];
        if let Err(e) = device.emit(&events) {
            error!("Emit error: {e}");
        }
        return;
    }

    let events = [
        InputEvent::new(EV_KEY, code, value),
        InputEvent::new(EV_SYN, SYN_REPORT, 0),
    ];
    if let Err(e) = device.emit(&events) {
        error!("Emit error: {e}");
    }
}

/// Discard every event already pending in the device's kernel ring buffer.
///
/// Grabbing a device does not flush its event buffer: a key press that was
/// in flight when the daemon took over (e.g. the Enter key used to start
/// the daemon itself) is still delivered to the grabbing process.  Forwarding
/// those stale events would inject a bare key-up without a matching press
/// into the virtual device and, for a still-held key, a burst of
/// auto-repeat taps.  The daemon's stream therefore starts clean at the
/// grab, and keys actually held at takeover are re-established by
/// [`capture_held_keys`] (engine state, sampled at the grab instant) and
/// [`sync_initial_state`] (the held modifiers' key-down re-emission) instead.
/// Note that this only covers events that were already buffered: the
/// repeats and release the kernel still generates after the grab for a key
/// that is physically held at takeover are handled by the engine's stale-key
/// tracking, not here.
///
/// The read inside `fetch_events` already removed the batch from the ring,
/// so the returned iterator may be dropped unprocessed; the only thing of
/// interest is the count of discarded events, which the caller uses to
/// decide whether a grace window is needed (see
/// [`native_release_window`]).
pub(super) fn drain_pending_events(device: &mut Device, path: &str) -> usize {
    let mut discarded = 0;
    loop {
        match device.fetch_events() {
            Ok(batch) => {
                let count = batch.count();
                discarded += count;
                // A zero-length read means the device went away mid-drain;
                // stop rather than spin on the end of the stream.
                if count == 0 {
                    break;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {
                continue;
            }
            Err(e) => {
                warn!("Linux: error draining pending events from {path}: {e}");
                break;
            }
        }
    }
    if discarded > 0 {
        debug!(
            "Linux: discarded {discarded} stale event(s) from before the \
             grab on {path}"
        );
    }
    discarded
}

/// Note every key currently held on the physical keyboard in the engine and
/// queue the held modifiers for re-emission.
///
/// Returns the grab-time snapshot of held key codes, which the caller feeds
/// to [`native_release_window`].
///
/// Runs immediately after the grab and the drain, while the kernel's key
/// state still reflects the grab instant itself.  Sampling later — after the
/// virtual device is built, for instance — misses a key that was released in
/// the meantime: its auto-repeat tail, generated between the grab and the
/// release, is still buffered in the device ring, and a repeat for a key the
/// engine never saw a key-down for is decided as a fresh press.  Each of
/// those repeats would be forwarded as a press+release tap; for the Return
/// key that started the daemon, that is a burst of extra newlines.
///
/// Held **modifiers** are noted as forwarded in the engine (restoring the
/// lookup bit and marking the key so its release is forwarded) and queued in
/// `pending_held_modifiers` for their key-down re-emission once the virtual
/// device exists.  Held **non-modifiers** are only noted as stale: their
/// key-down was already consumed by the client before the grab, so
/// re-emitting it — plus the auto-repeat tail and the release that follow —
/// would inject a second press.  The engine swallows their repeats and
/// forwards their release, which balances the pre-grab key-down.
pub(super) fn capture_held_keys(managed: &mut ManagedDevice) -> Vec<u16> {
    let Ok(held) = read_kernel_key_state(managed.device.as_raw_fd()) else {
        warn!(
            "Linux: failed to read the held key state of {} at grab time",
            managed.path
        );
        return Vec::new();
    };
    let modifiers = note_held_keys(&mut managed.engine, &held);
    managed.pending_held_modifiers = modifiers;
    // Log the grab-time snapshot: it is the only record of which keys the
    // client already had before the grab, and it decides how their post-grab
    // repeats and release are handled (stale vs. forwarded), so a mis-timed
    // sample is visible in the journal instead of showing up later as phantom
    // key presses.
    if !held.is_empty() {
        debug!(
            "Linux: held key(s) at grab on {}: {} ({} modifier(s) queued for \
             key-down re-emission)",
            managed.path,
            format_key_codes(&held),
            managed.pending_held_modifiers.len()
        );
    }
    held
}

/// Note a grab-time held-key list in the engine.
///
/// Every resolvable key is noted as held (a modifier is marked forwarded
/// with its lookup bit set, a non-modifier stale), and a key the
/// translation table cannot resolve is marked stale by key code only, since
/// it never reaches the engine's decide path.
///
/// Returns the key codes of the held modifiers, which the caller queues for
/// their key-down re-emission on the virtual device.
pub(super) fn note_held_keys(
    engine: &mut MappingEngine<u16>,
    held: &[u16],
) -> Vec<u16> {
    let mut modifiers = Vec::new();
    for &code in held {
        match keycode_to_hid_usage(code) {
            Some(usage) => {
                engine.note_held_key(code, usage);
                if HidUsage::hid_usage_to_modifier_bit(usage).is_some() {
                    modifiers.push(code);
                }
            }
            None => {
                // No resolvable HID identity: track it stale by key code
                // only, so its auto-repeat tail is dropped and its release
                // forwarded like any other held key.
                engine.note_stale_key(code);
            }
        }
    }
    modifiers
}

/// How long the startup ungrab window waits for the release of a key held
/// at grab time; after the deadline, whatever is still held falls back to
/// the stale-release-forward behavior.
///
/// Generous on purpose: the key that started the daemon (typically Return)
/// is often held for a couple of seconds while the service comes up.  The
/// window exits shortly after the release is observed, so the timeout only
/// bounds the pathological case of a key still held long after startup;
/// while the window runs the keyboard is simply not grabbed, so the cost of
/// waiting is a keyboard that is briefly unmapped, not lost input.
const NATIVE_RELEASE_TIMEOUT: Duration = Duration::from_secs(10);

/// Grace window when nothing was held at grab time but the drain discarded
/// events: their release may still sit in the compositor's own buffer,
/// unreadable while the device is grabbed.
const NATIVE_RELEASE_GRACE: Duration = Duration::from_millis(200);

/// Briefly release the grab so grab-time key releases reach the compositor
/// natively, then re-grab.
///
/// The compositor only learns about a release from an event on the very
/// device it last saw the press on.  A release that the grab intercepts and
/// the daemon later re-emits on the virtual keyboard is a release for a key
/// the virtual keyboard never pressed: the compositor's state for the
/// physical keyboard desynchronizes from the kernel's, and the first
/// presses of that key on the virtual keyboard are dropped while it
/// reconciles (observed on wlroots: the first one or two post-startup
/// Return presses were swallowed).  Delivering the release natively on the
/// physical device keeps every state consistent, as on the platforms where
/// nothing is grabbed at all.
///
/// While ungrabbed, every event reaches this process and the compositor
/// (each evdev reader keeps its own copy), so the daemon reads and
/// discards here: forwarding would double-deliver.  A grab-time held key
/// whose release is observed is unnoted in the engine, so its key-down is
/// not re-emitted on the virtual device (the client already has the
/// complete pair from the physical device); a key still held when the
/// deadline passes keeps its note and falls back to the stale-release
/// forward.
pub(super) fn native_release_window(
    managed: &mut ManagedDevice,
    held_at_grab: Vec<u16>,
    drain_found_events: bool,
) {
    if held_at_grab.is_empty() && !drain_found_events {
        // Clean takeover: nothing held, nothing pending -- the
        // compositor's state is already complete, no window needed.
        return;
    }
    let timeout = if held_at_grab.is_empty() {
        NATIVE_RELEASE_GRACE
    } else {
        NATIVE_RELEASE_TIMEOUT
    };

    let has_held_keys = !held_at_grab.is_empty();
    let mut pending: Vec<u16> = held_at_grab;
    if managed.device.ungrab().is_err() {
        warn!(
            "Linux: failed to ungrab {} for the native release window; \
             falling back to the stale-release forward",
            managed.path
        );
        return;
    }
    debug!(
        "Linux: ungrabbed {} for {timeout:?} so grab-time releases flow \
         natively",
        managed.path
    );

    let deadline = Instant::now() + timeout;
    loop {
        // A held set that became empty is a full native release: a short
        // grace so the compositor's read loop picks the release up before
        // the grab returns.
        if has_held_keys && pending.is_empty() {
            thread::sleep(Duration::from_millis(50));
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        let batch = match managed.device.fetch_events() {
            Ok(batch) => batch,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {
                continue;
            }
            Err(e) => {
                warn!(
                    "Linux: error reading {} during the native release \
                     window: {e}",
                    managed.path
                );
                break;
            }
        };
        // Collect owned copies so the device borrow from `fetch_events`
        // ends before the engine is mutated below.
        let events: Vec<InputEvent> = batch.into_iter().collect();
        for event in events {
            // Every event is discarded here: the compositor received its
            // own copy natively, and forwarding would double-deliver.
            if event.event_type() == EventType::KEY
                && event.value() == 0
                && let Some(pos) =
                    pending.iter().position(|&code| code == event.code())
            {
                let code = pending.swap_remove(pos);
                unnote_natively_released(managed, code);
            }
        }
    }

    drain_pending_events(&mut managed.device, &managed.path);
    if let Err(e) = managed.device.grab() {
        warn!("Linux: failed to re-grab {}: {e}", managed.path);
    }
    if !pending.is_empty() {
        debug!(
            "Linux: still held after the native release window on {}: {} \
             (their release will be forwarded via the virtual keyboard)",
            managed.path,
            format_key_codes(&pending)
        );
    }
}

/// Undo the engine's held-key note for a key whose release was delivered
/// natively during the ungrab window.
fn unnote_natively_released(managed: &mut ManagedDevice, code: u16) {
    match keycode_to_hid_usage(code) {
        Some(usage) => {
            managed.engine.unnote_held_key(code, usage);
            debug!(
                "Linux: {} released natively during the startup window on {}",
                usage.as_str(),
                managed.path
            );
        }
        None => {
            managed.engine.clear_stale_key(code);
            debug!(
                "Linux: key code {code} released natively during the startup \
                 window on {}",
                managed.path
            );
        }
    }
}

/// Re-emit the key-downs of the modifiers that were held when the device
/// was grabbed, completing the initial-state sync that
/// [`capture_held_keys`] started at grab time.
///
/// The engine was already noted at grab time; only the emission is deferred
/// until the virtual device exists (inline at startup, or on the device's
/// first pass when adopted by the hot-plug monitor).  Re-emitting from the
/// grab-time set — rather than re-reading the key state now — keeps a fresh
/// press made in the window between the grab and this point out of the
/// re-emission: its own key-down event is buffered in the ring and is
/// forwarded as usual.  A modifier released in that window still gets its
/// key-down re-emitted: the matching key-up is either already buffered in
/// the ring (processed right after, forwarded, and balancing the press) or
/// still to come.  The re-emission is idempotent when the modifier is still
/// held (the client already has the physical key-down), and the engine's
/// modifier state starts correct, so the first real event is never looked
/// up against a stale (neutral) mask — the root cause of a held-at-start
/// modifier breaking later key combinations.
pub(super) fn sync_initial_state(
    managed: &mut ManagedDevice,
    virtual_device: &mut VirtualDevice,
) {
    emit_actions(virtual_device, &plan_initial_state(managed));
}

/// Plan the held-modifier key-down re-emissions that complete the
/// initial-state sync [`capture_held_keys`] started at grab time (see
/// [`sync_initial_state`] for the full rationale).
///
/// Split from the emission so the event loop can build the actions under the
/// managed-device lock and replay them in [`emit_actions`] outside it, right
/// alongside the device's first batch of processed events.
pub(super) fn plan_initial_state(
    managed: &mut ManagedDevice,
) -> Vec<EmitAction> {
    let mut actions = Vec::new();
    for &code in &managed.pending_held_modifiers {
        // A modifier released during the native release window was
        // unnoted: the client already has the complete down+up pair from
        // the physical device, and re-emitting the key-down would leave it
        // stuck on the virtual device.
        let Some(usage) = keycode_to_hid_usage(code) else {
            continue;
        };
        if !managed.engine.held_modifier_forwarded(usage) {
            continue;
        }
        debug!(
            "Linux: re-emitting held modifier key-down {} on {}",
            code, managed.path
        );
        actions.push(EmitAction::Forward { code, value: 1 });
    }
    managed.pending_held_modifiers.clear();
    actions
}

/// Render a list of evdev key codes for logging: each code's canonical HID
/// name, or the raw code when the translation table cannot resolve it.
fn format_key_codes(codes: &[u16]) -> String {
    codes
        .iter()
        .map(|&code| {
            keycode_to_hid_usage(code)
                .map(|usage| usage.as_str().to_string())
                .unwrap_or_else(|| format!("code {code}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build the `EVIOCGKEY` request word for a buffer of `buf_len` bytes.
///
/// `EVIOCGKEY = _IOC(_IOC_READ, 'E' (0x45), 0x18, buf_len)` using the generic
/// Linux ioctl layout: `nr` at bit 0, `type` at bit 8, `size` at bit 16,
/// and `dir` at bit 30.  The size field is 14 bits wide.
fn eviocgkey_request(buf_len: usize) -> libc::c_ulong {
    const IOC_READ: u64 = 2;
    const EVIOC_TYPE: u64 = 0x45; // 'E'
    const EVIOC_NR: u64 = 0x18;
    IOC_READ << 30
        | EVIOC_TYPE << 8
        | EVIOC_NR
        | ((buf_len as u64 & 0x3fff) << 16)
}

/// Read the set of currently-held key codes straight from the kernel via
/// `EVIOCGKEY`.  Returns the `KEY_*` codes the physical keyboard reports as
/// down at this instant.
fn read_kernel_key_state(fd: RawFd) -> std::io::Result<Vec<u16>> {
    const KEY_CNT: usize = 0x2fe; // KEY_MAX + 1
    let buf_len = KEY_CNT.div_ceil(8);
    let mut buf = vec![0u8; buf_len];
    let request = eviocgkey_request(buf_len);
    // Safety: `buf` is a valid writable buffer of exactly the length encoded
    // in the request word, and `fd` is the live evdev handle this device owns.
    let ret = unsafe { libc::ioctl(fd, request, buf.as_mut_ptr() as *mut _) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(parse_key_bitmap(&buf, KEY_CNT))
}

/// Decode a kernel `EVIOCGKEY` key bitmap (one bit per code, byte-major) into
/// the list of currently-held key codes, dropping any code beyond `key_cnt`.
fn parse_key_bitmap(buf: &[u8], key_cnt: usize) -> Vec<u16> {
    let mut held = Vec::new();
    for (i, &byte) in buf.iter().enumerate() {
        for bit in 0..8u8 {
            if byte & (1 << bit) != 0 {
                let code = (i * 8 + bit as usize) as u16;
                if code < key_cnt as u16 {
                    held.push(code);
                }
            }
        }
    }
    held
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Initial-state bitmap parsing
    // -----------------------------------------------------------------------

    #[test]
    fn parse_key_bitmap_sets_expected_codes() {
        // Bit 2 of byte 0 => code 2; bit 5 of byte 1 => code 13.
        let mut buf = [0u8; 2];
        buf[0] = 1 << 2;
        buf[1] = 1 << 5;
        assert_eq!(parse_key_bitmap(&buf, 100), vec![2u16, 13]);
    }

    #[test]
    fn parse_key_bitmap_ignores_codes_beyond_key_count() {
        // KEY_CNT = 5: codes 0..4 are valid; any higher bit must be dropped.
        // Byte 0 all ones = codes 0..7, of which only 0..4 are in range.
        let buf = [0xFFu8, 0u8];
        assert_eq!(parse_key_bitmap(&buf, 5), vec![0u16, 1, 2, 3, 4]);
    }

    #[test]
    fn parse_key_bitmap_empty_returns_nothing() {
        assert!(parse_key_bitmap(&[0u8; 4], 100).is_empty());
    }

    #[test]
    fn eviocgkey_request_matches_kernel_ioctl_layout() {
        // _IOC(_IOC_READ=2, 'E'=0x45, nr=0x18, size=96) =
        // (2 << 30) | (0x45 << 8) | (0x18 << 0) | (96 << 16).
        assert_eq!(eviocgkey_request(96), 0x80604518);
    }

    // -----------------------------------------------------------------------
    // Grab-time held-key capture
    // -----------------------------------------------------------------------

    #[test]
    fn note_held_keys_notes_engine_and_collects_modifiers() {
        use std::sync::Arc;

        use parking_lot::RwLock;

        use crate::daemon::{state::Lookup, test_lookup::TestLookup};

        // Grab-time held keys: Return (28), LeftCtrl (29), and 729, which
        // the translation table cannot resolve.
        let lookup: Arc<RwLock<dyn Lookup>> = Arc::new(RwLock::new(
            TestLookup::from_yaml("- mappings:\n    LeftControl+A: B"),
        ));
        let mut engine = MappingEngine::new(lookup);
        let held = vec![28u16, 29u16, 729u16];

        // Only the held modifier is queued for key-down re-emission.
        let modifiers = note_held_keys(&mut engine, &held);
        assert_eq!(modifiers, vec![29u16]);

        // The held non-modifier's auto-repeat is swallowed and its release
        // forwarded (it balances the pre-grab key-down)...
        assert_eq!(
            engine.decide(28, HidUsage::Return, true, None, true),
            Decision::Swallow { release: 0 }
        );
        assert_eq!(
            engine.decide(28, HidUsage::Return, false, None, true),
            Decision::Pass
        );
        // ...the held modifier's bit is active for rule lookup, so the chord
        // fires against the forwarded modifier...
        assert_eq!(
            engine.decide(30, HidUsage::A, true, None, true),
            Decision::Emit {
                release: 1,
                outputs: vec![NativeKey {
                    modifiers: 0,
                    usage: HidUsage::B
                }]
            }
        );
        // ...and its physical release is a consumed release (the output
        // device already dropped it when the trigger fired).
        assert_eq!(
            engine.decide(29, HidUsage::LeftControl, false, None, true),
            Decision::ConsumedRelease
        );
        // The unresolvable key is tracked stale by code only.
        assert!(engine.has_stale_key(729u16));
        engine.clear_stale_key(729u16);
        assert!(!engine.has_stale_key(729u16));
    }

    #[test]
    fn format_key_codes_renders_names_and_fallback() {
        // Resolvable codes render their canonical name; a code the table
        // cannot resolve falls back to the raw code.
        assert_eq!(format_key_codes(&[30u16, 729u16]), "A, code 729");
        assert_eq!(format_key_codes(&[]), "");
    }

    // -----------------------------------------------------------------------
    // Modifier keycode mapping tests
    // -----------------------------------------------------------------------
    //
    // Verifies that the modifier-bit-to-evdev-code path agrees with the
    // shared HID usage table.

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
}
