// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Platform backend: the single public boundary between the platform
//! platform layer and the code above it (the daemon, `cli`, the `test-util`
//! dev-dependency crate, and — for [`config_dir`],
//! [`console_user_config_dir`], and through the [`config_access`] facet —
//! `common`).  Everything else in this module, `console_user_home` among
//! it, is implementation detail behind that surface.
//!
//! The stable public surface that the code above may depend on is, per
//! platform:
//!
//! - every platform: `list_keyboards`, `start_mapping`, `config_dir` (the
//!   per-user configuration base directory, consumed by
//!   `common::config_path`), and `keycode_to_hid_usage` (the native-keycode to
//!   `HidUsage` translation used by the `keys probe` CLI command)
//! - linux: additionally `keycode` (the canonical `HidUsage` and evdev-keycode
//!   tables) and `VIRTUAL_KEYBOARD_NAME`
//! - macos: additionally `start_virtkbd` (the root virtkbdd daemon entry
//!   point)
//! - windows: (no platform-specific production export beyond the common set)
//!
//! Test-harness-only exports are gated behind the crate's `test-util` feature
//! (enabled by the `test-util` dev-dependency crate and, transitively, by the
//! `tests/` crate during a test build): `KarabinerClient` and
//! `INJECTION_KEYBOARD_IDENTITY` on macOS, and `Key` on Windows.  With the
//! feature off — as in a plain `cargo build` of the release binaries — they
//! are not part of the public surface, so the "test-util sees the same code as
//! the daemon" boundary is compiler-enforced rather than conventional.
//!
//! A uniform signature is not a uniform-behavior guarantee.  `list_keyboards`
//! and `start_mapping` share one signature across platforms.  `list_keyboards`
//! behavior is uniform (architecture review F8): `Ok` carries the discovered
//! keyboards and may be empty, `Err` means the enumeration itself failed.
//! `start_mapping` behavior still diverges: `keyboard_filter` is honored on
//! Linux, ignored on macOS (lookups pass `device_id = None`), and treated as
//! a global no-op on Windows.  Each platform's own `list_keyboards`/
//! `start_mapping` docs are the authority on these rules.
//!
//! On top of the capture/injection backends, this module also exports
//! [`app_identity`] — the active-application query used by the daemon's
//! rule matching and by `keymapper appnames`.  It is OS-specific code, so
//! it is homed here rather than in [`common`](crate::common); this keeps
//! `platform` the single home for platform-specific implementation.
//!
//! Layering rule: the dependency arrows run
//! `platform -> keymap_core -> common`. The platform layer drives its
//! capture path through the cross-platform mapping core
//! ([`crate::keymap_core`]) and the shared [`common`](crate::common)
//! types; it must never depend on [`daemon`](crate::daemon) (the daemon
//! orchestrates the platform backends, not the other way around).
//!
//! Module organization: every module in this tree splits along its axis
//! of variation.  Code that differs per OS behind one shared interface is
//! a facet: it is homed in a module named for the capability it exposes
//! ([`app_identity`], [`config_access`], [`endpoint`], [`logging`], the
//! [`backend`] contract), with the interface at the module root and the
//! per-OS implementations as children, split on the real fault line
//! (`endpoint/unix.rs`, `config_access/unix.rs`, and `logging/file.rs`
//! each serve two OSes, so not every facet splits three ways).  Code that is
//! private to a single OS lives inside that OS's module (`linux`, `macos`,
//! `windows`) and splits by concern there (`capture`, `keyboard`, `keycode`,
//! `mapping`, ...).  The OS modules are the leaves of the facet tree — reached
//! through the contract above — not a second organizing principle: each
//! facet's interface sits directly above its implementations, and each OS's
//! machinery, bindings, and `#[cfg]`s stay inside one subtree.
//!
//! The same module also exports three `pub(crate)` facets.  [`logging`] is
//! the OS-specific log destination (stderr/journal on Linux, a rotating
//! file on macOS and Windows) consumed by [`crate::daemon::logging`] so
//! the daemon holds no `#[cfg(target_os)]` sink branches.
//! [`config_access`] is the OS-specific hardened config-file access
//! (symlink-safe open, Unix parent-chain and ownership/mode checks)
//! consumed by [`crate::common::config_io`] so the reader holds no
//! `#[cfg(unix)]` enforcement branches.  [`backend`] is
//! the cross-platform device-I/O contract (`KeySource`/`Emitter`:
//! enumerate, observe, emit, suppress-echo, release-mask policy); all
//! three platforms implement it
//! (`LinuxBackend`/`MacOsBackend`/`WindowsBackend`,
//! `LinuxEmitter`/`MacOsEmitter`/`WindowsEmitter`) and their
//! `list_keyboards`/`start_mapping` exports above drive the contract.
//! Like [`endpoint`] all three facets are `pub(crate)`: only the daemon,
//! the CLI, and `common` drive them, so they are not part of the public
//! platform surface documented above.  Besides the facets, the module
//! re-exports one further `pub(crate)` item: [`console_user_config_dir`],
//! the console user's configuration base directory (real on macOS only,
//! `None` elsewhere), consumed by [`crate::common::config_path`].
//! macOS' `console_user_home` is not re-exported: it is private to the
//! macOS subtree and consulted only by `console_user_config_dir`.
//!
//! The `test-util` dev-dependency crate and `cli` may depend only on this
//! surface, never on the `pub(crate)` internals of the platform
//! module. Anything not re-exported here is private implementation
//! detail and may change without notice.

/// Application-identity queries (active app name, visible-app listing).
///
/// A platform facet rather than a capture/injection backend: it exposes a
/// two-function interface over per-OS implementations and is consumed by
/// the daemon (`keymapperd`) and the CLI (`keymapper appnames`).
pub mod app_identity;

/// The cross-platform device-I/O contract (`KeySource`/`Emitter`).
///
/// Names the five responsibilities every capture backend has — enumerate,
/// observe, emit, suppress-echo, release-mask policy — so per-platform
/// backends are judged against a contract instead of an example.  All
/// three platforms implement it (`platform::<os>::backend::*Backend` as
/// `KeySource`, `platform::<os>::mapping::*Emitter` as `Emitter`); the
/// `allow(dead_code)` off Linux covers the contract surfaces the macOS
/// batch delivery keeps inert (its `Emitter::emit` arm and the action
/// variants it never constructs).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod backend;

/// The OS-specific hardened access behind the config-file reader.
///
/// [`crate::common::config_io`] owns the policy and sequence of the
/// hardened config read; this private facet owns only the OS-specific
/// enforcement: the symlink-safe open and, on Unix, the
/// parent-directory-chain verification and the ownership/world-writable
/// checks. It is `pub(crate)` because only the shared reader drives it,
/// so it is not part of the public platform surface documented above.
pub(crate) mod config_access;

/// The OS-specific transport behind the daemon's runtime control endpoint.
///
/// The framed control protocol stays in [`crate::daemon::control`]; this
/// private facet owns only the unix socket and Windows named-pipe transports
/// and their kernel-credential auth, exposed through the `Endpoint` trait that
/// `daemon::control` consumes. It is `pub(crate)` because only the daemon
/// drives it, so it is not part of the public platform surface documented
/// above.
pub(crate) mod endpoint;

/// The OS-specific log destination behind the daemon's log sink.
///
/// The ftlog line format, level gate, and panic hook stay in
/// [`crate::daemon::logging`]; this private facet owns only *where* the log
/// bytes go and *whether* each line carries a timestamp, exposed through the
/// `LogSink` factory that `daemon::logging` consumes. It is `pub(crate)`
/// because only the daemon drives it, so it is not part of the public platform
/// surface documented above.
pub(crate) mod logging;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

// Only the public API surface is re-exported.  Internal helpers (signal
// handlers, static flags) stay private to the platform module.
#[cfg(target_os = "linux")]
pub(crate) use linux::LinuxBackend;
#[cfg(target_os = "linux")]
pub use linux::config_dir;
#[cfg(target_os = "linux")]
pub(crate) use linux::console_user_config_dir;
#[cfg(target_os = "linux")]
pub use linux::keycode;
#[cfg(target_os = "linux")]
pub use linux::{
    VIRTUAL_KEYBOARD_NAME, keycode_to_hid_usage, list_keyboards, start_mapping,
};
#[cfg(target_os = "macos")]
pub(crate) use macos::MacOsBackend;
#[cfg(target_os = "macos")]
pub use macos::config_dir;
#[cfg(target_os = "macos")]
pub(crate) use macos::console_user_config_dir;
// Test-harness-only surface (see the module docs): only reachable with
// the `test-util` feature on.
#[cfg(all(target_os = "macos", feature = "test-util"))]
pub use macos::{INJECTION_KEYBOARD_IDENTITY, KarabinerClient};
#[cfg(target_os = "macos")]
pub use macos::{
    keycode_to_hid_usage, list_keyboards, start_mapping, start_virtkbd,
};
#[cfg(all(target_os = "windows", feature = "test-util"))]
pub use windows::Key;
#[cfg(target_os = "windows")]
pub(crate) use windows::WindowsBackend;
#[cfg(target_os = "windows")]
pub(crate) use windows::console_user_config_dir;
#[cfg(target_os = "windows")]
pub use windows::{
    config_dir, keycode_to_hid_usage, list_keyboards, start_mapping,
};
