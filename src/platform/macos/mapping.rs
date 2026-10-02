// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Keyboard input capture via a CGEventTap on macOS.
//!
//! A `kCGHIDEventTap` observes every keyboard event at the earliest point in
//! the input pipeline.  Each event is translated to a [`HidUsage`] and handed
//! to the unified mapping engine: unmapped keys are passed through unchanged,
//! mapped keys are swallowed and their outputs handed to the virtkbdd IPC
//! client for re-emission through the DriverKit virtual keyboard.  The tap's
//! mach port is scheduled on the main CFRunLoop, which is polled until a
//! shutdown signal (SIGINT or SIGTERM) is received.
//!
//! The engine's [`Decision`] is interpreted according to this platform's
//! additive virtual-device architecture: the `release` masks are inert (the
//! virtual keyboard's modifier state is isolated from physical typing, so
//! there is nothing to release on the output device), modifier-key outputs
//! are tapped rather than held (a remapped modifier therefore does not modify
//! subsequent physical keys), and a consumed modifier's physical release is
//! passed through, because forwarded events never touch the virtual keyboard.
//! These emission semantics are the [`Emitter`] impl's [`CONSUMED_RELEASE`]
//! policy, the single definition of the platform's release-mask meaning.
//!
//! [`Emitter`]: crate::platform::backend::Emitter
//! [`CONSUMED_RELEASE`]: crate::platform::backend::Emitter::CONSUMED_RELEASE
//!
//! Because the DriverKit virtual keyboard is a hardware-level device, the keys
//! it emits re-enter the HID pipeline and are re-received by this tap.  An
//! [`EchoTracker`] predicts the echo of each emission and lets the callback
//! pass those events through without re-deciding them, so a mapped key's echo
//! is never re-mapped (which would make `Escape: Cmd+T` fire on the echo of an
//! earlier `RightControl: Escape`, and cyclic rules emit forever).
//!
//! This runs in the user domain (keymapperd): a CGEventTap requires a
//! WindowServer connection, which a root daemon cannot have.  It needs the
//! Input Monitoring and Accessibility TCC grants; without them, tap creation
//! fails and a clear, actionable error is logged.
//!
//! This module also implements the emission half of the cross-platform
//! device-I/O contract ([`crate::platform::backend`]): [`MacOsEmitter`]
//! owns the virtkbdd batch sender and the [`EchoTracker`], and the tap
//! callback drives its capture decode through the shared
//! `capture::KeyScanner` — the same scanner the read-only observe pump
//! feeds, so probe and daemon cannot drift.

use std::{
    collections::VecDeque,
    ffi::c_void,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use log::info;
use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType,
};
use parking_lot::{Mutex, RwLock};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag::register,
};

use super::{capture::KeyScanner, ipc_client::IpcClient};
use crate::{
    common::{
        hid_usage::{HidUsage, PAGE_KEYBOARD},
        keyboard::KeyboardSpecifier,
        modifier::ModifierRole,
    },
    keymap_core::{
        emission::{ConsumedReleaseFate, InputFate, emission_plan},
        engine::{Decision, MappingEngine},
        logfmt::{self, Direction},
        lookup::Lookup,
        mapping_cache::NativeKey,
    },
    platform::backend::{Emitter, OutputAction},
};

/// Start keyboard input capture via a CGEventTap.
///
/// Creates a `kCGHIDEventTap` that observes keyboard events, decides each one
/// with the unified mapping engine, and re-emits mapped outputs through the
/// virtkbdd IPC client.  The CFRunLoop is polled until a shutdown signal
/// (SIGINT or SIGTERM) is received.
///
/// `keyboard_filter` is accepted for a uniform platform signature but ignored
/// in this phase: CGEvents do not expose the originating device, so lookups
/// pass `device_id = None` and per-keyboard filters are skipped.
pub fn start_mapping(
    lookup: Arc<RwLock<dyn Lookup>>,
    #[allow(unused_variables)] keyboard_filter: Option<Vec<KeyboardSpecifier>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Register signal handlers for graceful shutdown.
    let shutdown = Arc::new(AtomicBool::new(false));
    register(SIGINT, shutdown.clone())
        .expect("failed to register SIGINT handler");
    register(SIGTERM, shutdown.clone())
        .expect("failed to register SIGTERM handler");

    // Start the virtkbdd IPC client (spawns the writer thread).  When
    // virtkbdd is unreachable the reachability flag stays false and every key
    // passes through natively, so a dead emitter never breaks typing.
    let ipc = IpcClient::start()?;

    // Create the mapping engine.  `device_id` is `None` on macOS because
    // CGEvents cannot be correlated with an IOKit device.
    let engine = MappingEngine::new(lookup);

    // Build the tap context.  The callback is a plain function pointer, so all
    // state travels through the refcon; the context is a local that stays
    // alive for the run loop's duration, and its address is passed as the
    // refcon.  It is only ever touched on the main run-loop thread, so no
    // `Arc` (and thus no `Send + Sync`) is required.  The tap port is a
    // placeholder here and stored after creation (no events flow until the
    // run loop starts, so this is race-free).
    let mut ctx = TapContext {
        engine: Mutex::new(engine),
        emitter: MacOsEmitter {
            tx: ipc.sender(),
            echo: EchoTracker::new(),
        },
        reachable: ipc.reachable_flag(),
        tap_port: std::ptr::null(),
        scanner: KeyScanner::new(),
    };
    let refcon = &mut ctx as *mut TapContext as *mut c_void;

    // Observe key-down, key-up, and modifier (flags-changed) events at the
    // earliest tap point, so a swallowed event never reaches the WindowServer.
    // `CGEventMask` is a bitmask whose bit N selects event type N, so each
    // type must be shifted into its own bit.  A plain OR of the raw type
    // values (10 | 11 | 12 = 0xF) would select the low-numbered mouse events
    // and the tap would never see a single keyboard event.
    let mask: CGEventMask = (1u64 << CGEventType::KeyDown.0)
        | (1u64 << CGEventType::KeyUp.0)
        | (1u64 << CGEventType::FlagsChanged.0);

    let tap_port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(tap_callback),
            refcon,
        )
    }
    .ok_or_else(|| {
        "failed to create the CGEventTap. Grant keymapperd Input Monitoring \
         and Accessibility access in System Settings → Privacy & Security, \
         then restart it."
            .to_string()
    })?;

    // Store the port so the callback can re-enable the tap if the system
    // disables it.  Safe: no events flow until the run loop starts below.
    ctx.tap_port = &*tap_port as *const CFMachPort;

    // Schedule the tap's mach port on the main run loop so its callbacks fire.
    let source = CFMachPort::new_run_loop_source(None, Some(&tap_port), 0)
        .ok_or("failed to create the tap run-loop source")?;
    CFRunLoop::current()
        .ok_or("failed to get the current run loop")?
        .add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });

    // A CGEventTap is created disabled; it must be explicitly enabled or it
    // never receives events and every key passes through unmapped.  Enable it
    // now that its port is scheduled, so the callback can fire as soon as the
    // run loop starts below.
    CGEvent::tap_enable(&tap_port, true);

    run_event_loop(&shutdown);

    // The context and port are dropped here, after the run loop has ended, so
    // no callback can fire on freed memory.  The OS removes the tap when the
    // process exits.
    Ok(())
}

/// Poll the CFRunLoop until the shutdown flag is set.
fn run_event_loop(shutdown: &Arc<AtomicBool>) {
    // `kCFRunLoopDefaultMode` is a member of the common modes set and receives
    // the tap's mach-port callbacks.
    while !shutdown.load(Ordering::Acquire) {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.5, true);
    }

    info!("Shutdown signal received. Cleaning up...");
}

/// A single predicted echo event: a keyboard-page usage and its down/up state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EchoEvent {
    /// The HID usage of the echoed key.
    usage: HidUsage,
    /// Whether the echo is a press (`true`) or a release (`false`).
    is_down: bool,
}

/// The maximum time an echo entry stays pending before it is considered stale.
///
/// Emitted keys travel through the virtkbdd IPC client and the DriverKit
/// virtual keyboard before re-entering the HID pipeline as a `CGEvent`.  This
/// window bounds how long we wait for that round trip; an entry older than
/// this is dropped from the front of the queue.
const ECHO_WINDOW: Duration = Duration::from_millis(500);

/// Predicts and matches the echo of keys we emit through the virtual keyboard.
///
/// The DriverKit virtual keyboard is a hardware-level device: the keys it
/// emits re-enter the HID pipeline and are re-received by our own
/// `CGEventTap`.  Left unchecked, a mapped key's echo would be re-decided by
/// the engine, so a rule like `Escape: Cmd+T` would fire on the echo of an
/// earlier `RightControl: Escape`, and cyclic rules (`A: B` + `B: A`) would
/// emit forever.  This tracker records the exact sequence of `(usage,
/// is_down)` events each emission will produce (mirroring
/// `emit::keyboard_report_sequence`) and lets the tap callback recognize and
/// pass them through, bypassing the engine.
///
/// The echo is the emitted key's *only* delivery to the application, so a
/// match must pass the event through (not swallow it) — only the engine
/// decision is skipped.  Touched only on the main run-loop thread (the tap
/// callback), so no synchronization is needed.
struct EchoTracker {
    /// Pending echo events, in the order they will arrive at the tap.  Each
    /// is stamped with the time it was recorded, so stale entries can
    /// expire.
    pending: VecDeque<(Instant, EchoEvent)>,
}

impl EchoTracker {
    /// Create an empty tracker.
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
        }
    }

    /// Record the predicted echo sequence for a batch of emitted outputs.
    ///
    /// For each keyboard-page output, the echo is: each output modifier down
    /// (ascending bit order), the base key down, the base key up, then each
    /// output modifier up (descending bit order) — the same order as
    /// `emit::keyboard_report_sequence`, so the predicted sequence matches the
    /// order the events actually arrive at the tap.  Consumer-page outputs are
    /// skipped: they never re-enter the tap as a keyboard event, so an entry
    /// for them would only clog the queue head.
    fn record(&mut self, outputs: &[NativeKey], now: Instant) {
        for native_key in outputs {
            if native_key.usage.page() != PAGE_KEYBOARD {
                continue;
            }
            let base = native_key.usage;
            let modifiers = native_key.modifiers;

            // Press each output modifier, one at a time in ascending bit
            // order.
            for bit in 0..8 {
                if (modifiers >> bit) & 1 == 1 {
                    let Some(usage) = modifier_usage(bit) else {
                        continue;
                    };
                    self.pending.push_back((
                        now,
                        EchoEvent {
                            usage,
                            is_down: true,
                        },
                    ));
                }
            }

            // Press and release the base key.
            self.pending.push_back((
                now,
                EchoEvent {
                    usage: base,
                    is_down: true,
                },
            ));
            self.pending.push_back((
                now,
                EchoEvent {
                    usage: base,
                    is_down: false,
                },
            ));

            // Release each output modifier, one at a time in descending bit
            // order.
            for bit in (0..8).rev() {
                if (modifiers >> bit) & 1 == 1 {
                    let Some(usage) = modifier_usage(bit) else {
                        continue;
                    };
                    self.pending.push_back((
                        now,
                        EchoEvent {
                            usage,
                            is_down: false,
                        },
                    ));
                }
            }
        }
    }

    /// Check whether the next pending echo matches `(usage, is_down)`.
    ///
    /// Expired entries (older than [`ECHO_WINDOW`]) are dropped from the front
    /// first.  If the new head matches, it is consumed and `true` is returned;
    /// otherwise nothing is consumed and `false` is returned.  A mismatch does
    /// not consume, so an interleaved user event passes to the engine and the
    /// echo still matches when it arrives.
    fn matches(
        &mut self,
        usage: HidUsage,
        is_down: bool,
        now: Instant,
    ) -> bool {
        // Drop stale entries from the front.
        while let Some((recorded, _)) = self.pending.front() {
            if now.duration_since(*recorded) > ECHO_WINDOW {
                self.pending.pop_front();
            } else {
                break;
            }
        }

        match self.pending.front() {
            Some((_, head)) if *head == EchoEvent { usage, is_down } => {
                self.pending.pop_front();
                true
            }
            _ => false,
        }
    }
}

/// Map a modifier bit position to its keyboard-page [`HidUsage`].
fn modifier_usage(bit: u8) -> Option<HidUsage> {
    ModifierRole::try_from_bit(bit)
        .and_then(|role| HidUsage::keyboard(role.hid_id()))
}

/// The macOS output device: the DriverKit virtual keyboard behind the
/// virtkbdd IPC channel.
///
/// The [`Emitter`] impl is the single home of the platform's emission
/// semantics.  It differs structurally from the Linux one: macOS emits
/// *batches* (one IPC frame per engine decision) rather than individual
/// key events, because the batch doubles as the [`EchoTracker`]'s
/// prediction unit — recording the whole predicted sequence at once is
/// what keeps a multi-output mapping's echo matched in order.
/// [`MacOsEmitter::emit`] therefore acts on a single [`OutputAction`],
/// while the tap callback delivers mapped outputs through
/// [`MacOsEmitter::emit_batch`].
pub(super) struct MacOsEmitter {
    /// The virtkbdd batch sender (fire-and-forget).
    tx: mpsc::SyncSender<Vec<NativeKey>>,
    /// Predicts and matches the echo of keys emitted through the virtual
    /// keyboard, so they pass through the tap without being re-decided by the
    /// engine.  Touched only on the main run-loop thread, so no
    /// synchronization is needed.
    echo: EchoTracker,
}

impl Emitter for MacOsEmitter {
    /// The virtual keyboard's modifier state is isolated from physical
    /// typing: a consumed modifier's physical release is passed through
    /// because forwarded events never reach the virtual keyboard, which
    /// never held the modifier in the first place.
    const CONSUMED_RELEASE: ConsumedReleaseFate =
        ConsumedReleaseFate::PassThrough;

    fn emit(&mut self, action: &OutputAction) {
        match action {
            // Unmapped keys pass through the tap natively; the virtual
            // keyboard never sees them.
            OutputAction::Forward { .. } => {}
            // The release mask is inert on this platform (see
            // `CONSUMED_RELEASE`).
            OutputAction::ReleaseConsumed { .. } => {}
            // macOS taps every output, including modifier-key outputs:
            // a remapped modifier must not modify subsequent physical
            // keys, so a `Hold` is delivered as a `Tap` (see the module
            // docs).
            OutputAction::Tap { native_key }
            | OutputAction::Hold { native_key } => {
                self.emit_batch(std::slice::from_ref(native_key));
            }
        }
    }
}

impl MacOsEmitter {
    /// Deliver one mapped-output batch to virtkbdd and record its
    /// predicted echo.
    ///
    /// Fire-and-forget; a dropped batch (channel full) produces no echo,
    /// so nothing is recorded.
    fn emit_batch(&mut self, outputs: &[NativeKey]) {
        if self.tx.try_send(outputs.to_vec()).is_ok() {
            self.echo.record(outputs, Instant::now());
        }
    }
}

/// The state shared with the CGEventTap callback via its refcon.
struct TapContext {
    /// The mapping engine, guarded because `decide` takes `&mut self`.
    engine: Mutex<MappingEngine<HidUsage>>,
    /// The platform emitter: virtkbdd batch sender and echo tracker.
    emitter: MacOsEmitter,
    /// The virtkbdd reachability flag.
    reachable: Arc<AtomicBool>,
    /// The tap's mach port, so the callback can re-enable a disabled tap.
    /// Set after `tap_create`, before the run loop starts.
    tap_port: *const CFMachPort,
    /// Decodes raw CGEvents into [`CapturedKey`]s for the engine, sharing
    /// the decode step with the read-only observe pump.  Touched only on
    /// the main run-loop thread, so no synchronization is needed.
    ///
    /// [`CapturedKey`]: crate::platform::backend::CapturedKey
    scanner: KeyScanner,
}

/// The CGEventTap callback.
///
/// Runs on the main run-loop thread and must stay fast: a lock-read lookup
/// plus a `try_send`, no I/O.  Returns the event to pass it through, or null
/// to swallow it.
unsafe extern "C-unwind" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    refcon: *mut c_void,
) -> *mut CGEvent {
    // Mutable because the callback updates the CapsLock previous-state
    // tracker; it runs only on the main run-loop thread, so no aliasing.
    let ctx = unsafe { &mut *(refcon as *mut TapContext) };

    // The system disables taps that block (or when secure input is active).
    // Re-enable and pass the event through.
    if event_type == CGEventType::TapDisabledByTimeout
        || event_type == CGEventType::TapDisabledByUserInput
    {
        if !ctx.tap_port.is_null() {
            unsafe { CGEvent::tap_enable(&*ctx.tap_port, true) };
        }
        return event.as_ptr();
    }

    // Decode through the shared scanner: keycode to HID usage, direction
    // from the event type (with the CapsLock toggle quirk).  Events it
    // cannot classify (mouse events, Fn's flags-changed) and keycodes
    // with no HID equivalent (media keys, F13+) pass through untouched.
    let Some(captured) =
        ctx.scanner.on_event(event_type, unsafe { event.as_ref() })
    else {
        return event.as_ptr();
    };
    let Some(usage) = captured.usage else {
        return event.as_ptr();
    };
    // macOS auto-repeat arrives as repeated key-downs, so the only
    // directions are down and up.
    let is_down = captured.direction() != Direction::Up;
    let keycode = captured.native;

    // The wording and level of the `recv`/`pass`/`swal` lines are owned by
    // `logfmt`, so the e2e debug-log grammar has one producing implementation
    // shared with the other backends.
    let dir = Direction::from_is_down(is_down);
    logfmt::log_recv(format_args!("keycode={keycode}"), dir, usage);

    // The virtual keyboard is a hardware-level device: the keys it emits
    // re-enter the HID pipeline and are re-received by this tap.  If the event
    // is the predicted echo of a key we just emitted, pass it through without
    // re-deciding it — the echo is the key's only delivery to the application,
    // so it must reach the app, but it must not be re-mapped (which would make
    // `Escape: Cmd+T` fire on the echo of an earlier `RightControl: Escape`,
    // and cyclic rules emit forever).
    if ctx.emitter.echo.matches(usage, is_down, Instant::now()) {
        logfmt::log_pass(format_args!("keycode={keycode}"), dir, usage);
        return event.as_ptr();
    }

    let reachable = ctx.reachable.load(Ordering::Acquire);
    let decision = {
        let mut e = ctx.engine.lock();
        // The HID usage is the key identity: it is page-specific and
        // unambiguous, and CGEvents expose no finer-grained identity.
        e.decide(usage, usage, is_down, None, reachable)
    };

    // The shared emission plan owns the `Decision` semantics and logs each
    // output's `emit` line.  macOS reads the plan only for the fate of the
    // input event: its additive virtual keyboard taps every output (the plan's
    // tap/hold split and release mask are inert here, matching the
    // [`MacOsEmitter`] impl), and a consumed modifier's release passes through
    // per the [`Emitter::CONSUMED_RELEASE`] policy.  The mapped batch is
    // delivered through the emitter from the decision itself.
    let plan = emission_plan(&decision, MacOsEmitter::CONSUMED_RELEASE);

    match plan.input {
        InputFate::Forward => {
            logfmt::log_pass(format_args!("keycode={keycode}"), dir, usage);
            event.as_ptr()
        }
        InputFate::Swallow => {
            logfmt::log_swal(format_args!("keycode={keycode}"), dir, usage);
            std::ptr::null_mut()
        }
        // A mapped key: swallow the input (the `emit` lines stand for it) and
        // hand the outputs to the emitter as one batch.
        InputFate::Silent => {
            if let Decision::Emit { outputs, .. } = &decision {
                ctx.emitter.emit_batch(outputs);
            }
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The release-mask policy pinned at the contract's single definition
    /// site (F2b): a consumed modifier's physical release passes through,
    /// because the virtual keyboard's modifier state is isolated from
    /// physical typing — swallowing the physical release would leave the
    /// *system* modifier state stuck.
    ///
    /// (The flag-mask and CapsLock direction logic these tests used to
    /// cover moved to the shared `capture::KeyScanner` and is pinned
    /// there.)
    #[test]
    fn emitter_declares_pass_through_consumed_release() {
        assert_eq!(
            MacOsEmitter::CONSUMED_RELEASE,
            ConsumedReleaseFate::PassThrough
        );
    }

    /// A bare key (no modifiers) echoes as a down/up pair.
    #[test]
    fn echo_bare_key_sequence() {
        let mut tracker = EchoTracker::new();
        let now = Instant::now();
        tracker.record(
            &[NativeKey {
                modifiers: 0,
                usage: HidUsage::A,
            }],
            now,
        );

        assert!(tracker.matches(HidUsage::A, true, now));
        assert!(tracker.matches(HidUsage::A, false, now));
        // The queue is now empty.
        assert!(!tracker.matches(HidUsage::A, true, now));
    }

    /// A chord (modifier + base key) echoes in the canonical order: modifier
    /// down, base down, base up, modifier up.
    #[test]
    fn echo_chord_order() {
        let mut tracker = EchoTracker::new();
        let now = Instant::now();
        tracker.record(
            &[NativeKey {
                modifiers: 0x08,
                usage: HidUsage::T,
            }], // LeftCommand + T
            now,
        );

        assert!(tracker.matches(HidUsage::LeftCommand, true, now));
        assert!(tracker.matches(HidUsage::T, true, now));
        assert!(tracker.matches(HidUsage::T, false, now));
        assert!(tracker.matches(HidUsage::LeftCommand, false, now));
        // The queue is now empty.
        assert!(!tracker.matches(HidUsage::T, true, now));
    }

    /// An entry older than the echo window expires and is dropped from the
    /// front, so it no longer matches.
    #[test]
    fn echo_stale_expiry() {
        let mut tracker = EchoTracker::new();
        let now = Instant::now();
        tracker.record(
            &[NativeKey {
                modifiers: 0,
                usage: HidUsage::A,
            }],
            now,
        );

        // Within the window: the down matches.
        assert!(tracker.matches(
            HidUsage::A,
            true,
            now + Duration::from_millis(100)
        ));
        // The up is still pending.  Advance past the window: it expires.
        assert!(!tracker.matches(
            HidUsage::A,
            false,
            now + Duration::from_millis(600)
        ));
        // The queue is now empty.
        assert!(!tracker.matches(
            HidUsage::A,
            false,
            now + Duration::from_millis(600)
        ));
    }

    /// A mismatch does not consume the head, so the echo still matches when it
    /// arrives (an interleaved user event passes to the engine).
    #[test]
    fn echo_mismatch_does_not_consume() {
        let mut tracker = EchoTracker::new();
        let now = Instant::now();
        tracker.record(
            &[NativeKey {
                modifiers: 0,
                usage: HidUsage::A,
            }],
            now,
        );

        // A different key does not match and does not consume.
        assert!(!tracker.matches(HidUsage::B, true, now));
        // The echo still matches.
        assert!(tracker.matches(HidUsage::A, true, now));
        assert!(tracker.matches(HidUsage::A, false, now));
    }

    /// A consumer-page output never re-enters the tap as a keyboard event, so
    /// no echo is recorded for it.
    #[test]
    fn echo_consumer_output_no_echo() {
        let mut tracker = EchoTracker::new();
        let now = Instant::now();
        tracker.record(
            &[NativeKey {
                modifiers: 0,
                usage: HidUsage::PlayPause,
            }],
            now,
        );

        // The queue is empty: no echo was recorded.
        assert!(!tracker.matches(HidUsage::PlayPause, true, now));
    }

    /// A modifier key used as the base (no output modifiers) echoes as a
    /// down/up pair of that modifier.
    #[test]
    fn echo_modifier_base_output() {
        let mut tracker = EchoTracker::new();
        let now = Instant::now();
        tracker.record(
            &[NativeKey {
                modifiers: 0,
                usage: HidUsage::LeftControl,
            }],
            now,
        );

        assert!(tracker.matches(HidUsage::LeftControl, true, now));
        assert!(tracker.matches(HidUsage::LeftControl, false, now));
        // The queue is now empty.
        assert!(!tracker.matches(HidUsage::LeftControl, true, now));
    }
}
