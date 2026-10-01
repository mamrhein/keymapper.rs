// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Platform backend: the single public boundary between the platform
//! layer and the code above it (the daemon, `cli`, the `test-util`
//! dev-dependency crate, and — for [`config_dir`] and, on macOS,
//! `console_user_home` — `common`).
//!
//! The stable public surface that the code above may depend on is, per
//! platform:
//!
//! - every platform: `list_keyboards`, `start_mapping`, `config_dir` (the
//!   per-user configuration base directory, consumed by
//!   `common::config_path`), and `keycode_to_hid_usage` (the native-keycode to
//!   `HidUsage` translation used by the `keys probe` CLI command)
//! - linux: additionally `hid_translate` (the canonical `HidUsage` and
//!   evdev-keycode tables) and `VIRTUAL_KEYBOARD_NAME`
//! - macos: additionally `start_virtkbd` (the root virtkbdd daemon entry
//!   point) and `console_user_home` (the console user's home directory,
//!   consumed by `common::config_path` when running as root)
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
//! and `start_mapping` share one signature across platforms, but their
//! behavior diverges: `keyboard_filter` is honored on Linux, ignored on macOS
//! (lookups pass `device_id = None`), and treated as a global no-op on
//! Windows; and the empty/no-hardware result differs per platform (Linux and
//! Windows return `Err`, macOS returns a placeholder).  Each platform's own
//! `list_keyboards`/`start_mapping` docs are the authority on these rules.
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
//! The same module also exports a `pub(crate)` [`logging`] facet — the
//! OS-specific log destination (stderr/journal on Linux, a rotating file on
//! macOS and Windows) consumed by [`crate::daemon::logging`] so the daemon
//! holds no `#[cfg(target_os)]` sink branches. Like [`endpoint`] it is
//! `pub(crate)`: only the daemon drives it, so it is not part of the public
//! platform surface documented above.
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
pub use linux::config_dir;
#[cfg(target_os = "linux")]
pub use linux::hid_translate;
#[cfg(target_os = "linux")]
pub use linux::{
    VIRTUAL_KEYBOARD_NAME, keycode_to_hid_usage, list_keyboards, start_mapping,
};
// Test-harness-only surface (see the module docs): only reachable with
// the `test-util` feature on.
#[cfg(all(target_os = "macos", feature = "test-util"))]
pub use macos::{INJECTION_KEYBOARD_IDENTITY, KarabinerClient};
#[cfg(target_os = "macos")]
pub use macos::{config_dir, console_user_home};
#[cfg(target_os = "macos")]
pub use macos::{
    keycode_to_hid_usage, list_keyboards, start_mapping, start_virtkbd,
};
#[cfg(all(target_os = "windows", feature = "test-util"))]
pub use windows::Key;
#[cfg(target_os = "windows")]
pub use windows::{
    config_dir, keycode_to_hid_usage, list_keyboards, start_mapping,
};
