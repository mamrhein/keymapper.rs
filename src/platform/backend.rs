// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The cross-platform device-I/O contract: what every platform backend
//! must be able to do, independent of how it does it.
//!
//! The three capture stacks used to be structurally unrelated: each
//! platform exported free functions (`list_keyboards`, `start_mapping`)
//! whose behavior diverged in undocumented ways, each solved self-echo
//! suppression with a different (unnameable) mechanism, and each
//! re-interpreted the engine's release decisions with a hardcoded policy
//! at its own `emission_plan` call site.  This module names the five
//! responsibilities every backend actually has, so a new platform can be
//! judged against a checklist instead of an example:
//!
//! - **enumerate** — list the observable keyboards
//!   ([`KeySource::list_keyboards`]).
//! - **observe** — capture key events with a shared decode step, in a
//!   read-only mode the `keymapper keys probe` command consumes
//!   ([`KeySource::observe`]) as well as the daemon's own capture loop.
//! - **emit** — deliver the shared emission plan's actions to the platform's
//!   output device ([`Emitter::emit`]).
//! - **suppress-echo** — never treat the daemon's own emitted events as input.
//!   Where the OS models the output device as an enumerable input device, the
//!   policy is expressible as a predicate ([`KeySource::is_output_device`]);
//!   where it does not, the mechanism stays inside the capture path (macOS
//!   predictive echo tracking, the Windows injected-event tag) and the
//!   predicate simply answers `false`.
//! - **release-mask policy** — declare what a `Decision::ConsumedRelease`
//!   means on this platform's output device ([`Emitter::CONSUMED_RELEASE`]),
//!   instead of every backend hardcoding a [`ConsumedReleaseFate`] at its call
//!   site.
//!
//! Porting status: all three platforms implement the contract —
//! Linux (`platform::linux::backend::LinuxBackend` as [`KeySource`],
//! `platform::linux::mapping::LinuxEmitter` as [`Emitter`]), macOS
//! (`platform::macos::backend::MacOsBackend`,
//! `platform::macos::mapping::MacOsEmitter`), and Windows
//! (`platform::windows::backend::WindowsBackend`,
//! `platform::windows::mapping::WindowsEmitter`) — one platform per
//! increment, so the contract was shaped by three real
//! implementations rather than one plus two stubs.  On every platform
//! the uniform `list_keyboards`/`start_mapping` exports in
//! [`crate::platform`] are thin shims that drive this contract, so the
//! trait — not a parallel set of free functions — is the definition of
//! the platform boundary the daemon binary and CLI consume.

use std::{error::Error, sync::Arc};

use parking_lot::RwLock;

use crate::{
    common::{
        hid_usage::HidUsage,
        keyboard::{KeyboardInfo, KeyboardSpecifier},
    },
    keymap_core::{
        emission::ConsumedReleaseFate, logfmt::Direction, lookup::Lookup,
        mapping_cache::NativeKey,
    },
};

/// A key event decoded by a platform's capture path.
///
/// It is what both the daemon's grabbed capture and the read-only observe
/// mode hand upward: the native code as seen by the OS, the press
/// direction, and the resolved HID identity (or `None` when the
/// platform's translation table cannot resolve the code — callers decide
/// what an unknown key means: the probe prints `Unknown(code)`, the
/// daemon forwards the raw event).
#[derive(Debug, Clone, Copy)]
pub(crate) struct CapturedKey {
    /// The OS-native key code: an evdev `KEY_*` code on Linux, a
    /// `CGKeyCode` on macOS, a virtual-key code on Windows.
    pub native: u16,
    /// The evdev-style value: `0` release, `1` press, `2` auto-repeat.
    pub value: i32,
    /// The resolved HID identity, `None` for codes the platform table
    /// cannot resolve.
    pub usage: Option<HidUsage>,
}

impl CapturedKey {
    /// The press direction of this event.
    pub fn direction(self) -> Direction {
        match self.value {
            0 => Direction::Up,
            1 => Direction::Down,
            _ => Direction::Repeat,
        }
    }
}

/// A deferred output action for an [`Emitter`].
///
/// The capture path decides these actions (the shared
/// [`emission_plan`](crate::keymap_core::emission::emission_plan) plus the
/// backend's own [`Forward`](OutputAction::Forward) events) and the
/// event loop executes them later, outside any lock, so the pacing
/// between sub-events never blocks other work.
#[derive(Debug)]
pub(crate) enum OutputAction {
    /// Forward a raw key event unchanged: an unmapped press/release, the
    /// auto-repeat of an unmapped key, or the re-emitted key-down of a
    /// modifier held at grab time.
    Forward { native: u16, value: i32 },
    /// Release the fired trigger's consumed modifier bits before its
    /// output, so the output is a clean tap.
    ReleaseConsumed { consumed: u8 },
    /// Emit a self-contained mapped tap (modifiers, base, releases).
    Tap { native_key: NativeKey },
    /// Hold down a mapped modifier-key output on the output device until
    /// the physical key-up.
    Hold { native_key: NativeKey },
}

/// The device-side half of the contract: enumerate, observe, and run the
/// capture-and-emit runtime.
///
/// The trait is not dyn-dispatched: each platform compiles exactly one
/// implementation, and the code above selects it by `#[cfg]`.
pub(crate) trait KeySource {
    /// Enumerate the keyboard devices this platform can observe.
    ///
    /// The empty/no-hardware semantics are currently per-platform
    /// (architecture review F8, tracked as its own Phase 3 item).
    fn list_keyboards(&self) -> Result<Vec<KeyboardInfo>, Box<dyn Error>>;

    /// Whether a device with this name is the daemon's own emission
    /// device, which capture must never observe (echo suppression).
    fn is_output_device(&self, name: &str) -> bool;

    /// Open `keyboard` in read-only observe mode (never grabbed) and
    /// pump every decoded key event through `on_key`.
    ///
    /// This is the capture mode `keymapper keys probe` consumes while
    /// the daemon is *not* running; the daemon's own grabbed capture
    /// shares the scanner that decodes these events.  The pump runs
    /// until the process terminates; the `Err` path reports only a
    /// failure to open the device.
    fn observe(
        &self,
        keyboard: &KeyboardInfo,
        on_key: &mut dyn FnMut(CapturedKey),
    ) -> Result<(), Box<dyn Error>>;

    /// Start the platform's capture → map → emit runtime, blocking for
    /// the lifetime of the daemon.
    fn start_mapping(
        &self,
        lookup: Arc<RwLock<dyn Lookup>>,
        keyboard_filter: Option<Vec<KeyboardSpecifier>>,
    ) -> Result<(), Box<dyn Error>>;
}

/// The output-side half of the contract: what the platform's emission
/// device means and how actions reach it.
pub(crate) trait Emitter {
    /// How this platform's output device treats a
    /// [`Decision::ConsumedRelease`]: [`ConsumedReleaseFate::Swallow`]
    /// when the device already released the modifier when the trigger
    /// fired, [`ConsumedReleaseFate::PassThrough`] when forwarded events
    /// never reach the output device.
    ///
    /// Capture code reads its policy from here, so the release-mask
    /// meaning has exactly one definition per platform.
    const CONSUMED_RELEASE: ConsumedReleaseFate;

    /// Execute one deferred output action against the output device.
    fn emit(&mut self, action: &OutputAction);
}
