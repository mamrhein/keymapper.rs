// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! End-to-end integration tests using file-based event validation.
//!
//! These tests spawn `keymapper_monitor` as a subprocess to capture the
//! daemon's output into a log file, inject synthetic key events via the
//! platform injector, and validate the daemon's remapped output against
//! expected sequences derived from the config fixture files.
//!
//! On every supported platform the monitor runs headless and captures the
//! daemon's output directly: on Linux it grabs the daemon's uinput output
//! device, on Windows a low-level hook captures the daemon's tagged
//! re-emissions, and on macOS it seizes the daemon's Karabiner DriverKit
//! virtual keyboard.  The capture is therefore deterministic, works on
//! interactive sessions, and the daemon's output can never leak into the
//! compositor or a focused window.  The daemon's active-app query is pinned
//! to the monitor's app name via `KEYMAPPER_ACTIVE_APP`, so app-scoped rules
//! are evaluated deterministically even though the monitor has no window.
//!
//! The test flow is:
//! 1. Create a temp directory and copy a fixture config into it.
//! 2. Parse the config and collect all trigger rules with their app scope.
//! 3. Build an injection sequence interleaving triggers with passthrough keys
//!    that no rule uses.
//! 4. Build the expected sequence by simulating the daemon's per-event
//!    behaviour (rule firing, swallow, passthrough, chord output taps) and the
//!    monitor's platform-specific key reporting.
//! 5. Start the monitor, injector, and daemon.
//! 6. Inject a canary key to verify the full capture path is live, then inject
//!    the sequence and assert the event log matches the expected sequence.
//!
//! The temp directory, the monitor process, and the daemon are wrapped in
//! RAII guards, so the environment is cleaned up even when a test fails.

mod common;
mod event_log;

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use common::E2eLock;
use event_log::{LogEvent, assert_events_match, event_str};
use keymapper::{
    common::{
        config::AppConfig,
        hid_usage::{HidUsage, PAGE_CONSUMER},
    },
    util::key_injector::{InjectorError, KeyInjector},
};

// ---------------------------------------------------------------------------
// e2e gate — e2e tests require elevated privileges
// ---------------------------------------------------------------------------

/// Cache the result of the e2e capability check.  We probe once at startup
/// rather than per-test, because the result is a global property of the
/// environment (Accessibility permission, HID driver availability, etc.).
static CAN_RUN_E2E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn can_run_e2e() -> bool {
    *CAN_RUN_E2E.get_or_init(|| {
        // Check that the injector can be created.
        create_injector().is_ok_and(|opt| opt.is_some())
    })
}

// ---------------------------------------------------------------------------
// Test fixture paths
// ---------------------------------------------------------------------------

/// Path to the comprehensive config fixture.  Contains mappings that
/// exercise single-key remaps, chord outputs, and modifier triggers.
const CONFIG_COMPREHENSIVE: &str =
    "tests/fixtures/configs/config_comprehensive.yaml";

/// Path to the reloaded config fixture.  Contains different mappings to
/// verify hot-reload behavior.
const CONFIG_RELOADED: &str = "tests/fixtures/configs/config_reloaded.yaml";

// ---------------------------------------------------------------------------
// Binary path resolution
// ---------------------------------------------------------------------------

/// Resolve the path to the compiled `keymapper_monitor` binary.
fn monitor_bin_path() -> PathBuf {
    env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("keymapper_monitor")
}

/// Resolve the path to the compiled `keymapper` CLI binary.
fn cli_bin_path() -> PathBuf {
    env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("keymapper")
}

// ---------------------------------------------------------------------------
// Config-driven sequence builders
// ---------------------------------------------------------------------------

/// Represents a single injection step in the test sequence.  Each step
/// injects a down event followed by an up event with a small delay between.
#[derive(Debug, Clone)]
struct InjectionStep {
    /// The native key codes to inject (modifiers first, then base key).
    keys_down: Vec<u16>,
    /// The native key codes to release (base key first, then modifiers).
    keys_up: Vec<u16>,
}

/// The result of parsing a config file for test sequence generation.
struct TestSequences {
    /// Ordered injection steps: triggers interleaved with passthrough keys.
    steps: Vec<InjectionStep>,
    /// Expected log events corresponding to each injection step.
    expected: Vec<LogEvent>,
}

/// Application name the monitor process reports as the active app while the
/// test runs.  App-scoped rules for this app are expected to fire; rules
/// scoped to any other app (e.g. "to_be_ignored") must not.
const MONITOR_APP_NAME: &str = "keymapper_monitor";

/// A rule collected from the config, annotated with its app scope.
struct CollectedRule<'a> {
    /// The trigger key event (base plus held modifiers).
    trigger: &'a keymapper::common::config::KeyEvent,
    /// The rule's output key events.
    outputs: Vec<&'a keymapper::common::config::KeyEvent>,
    /// App names the rule is scoped to; empty means global.
    apps: Vec<String>,
}

impl CollectedRule<'_> {
    /// Whether the rule is expected to fire while the monitor process is the
    /// active application.
    fn fires_for_monitor(&self) -> bool {
        self.apps.is_empty() || self.apps.iter().any(|a| a == MONITOR_APP_NAME)
    }
}

/// Build test sequences for one config phase.
///
/// When *config_path* is `Some`, the fixture at that path is parsed and all
/// trigger rules are collected (keeping app scope).  When it is `None`, an
/// empty config is used, so the sequence contains only passthrough keys.
///
/// Passthrough keys that no rule uses are interleaved with the triggers to
/// exercise both remapping and transparent forwarding.  The expected sequence
/// simulates the daemon's per-event behaviour (see [`rule_expected_events`])
/// and the monitor's reporting semantics (see [`monitor_key_name`]).
fn build_test_sequences(config_path: Option<&Path>) -> TestSequences {
    let app_config = match config_path {
        Some(path) => {
            let content = std::fs::read_to_string(path).unwrap_or_else(|e| {
                panic!("failed to read config fixture {path:?}: {e}")
            });
            AppConfig::load_from_str(&content).unwrap_or_else(|e| {
                panic!("failed to parse config fixture {path:?}: {e}")
            })
        }
        // No user config: parse an empty config so the sequence contains only
        // passthrough keys.
        None => AppConfig::load_from_str("groups: []")
            .expect("empty config must parse"),
    };

    // Collect all rules from every group, keeping app scope so firing
    // expectations can account for the active app.
    let mut rules: Vec<CollectedRule> = Vec::new();
    for group in &app_config.groups {
        for (trigger, output_events) in group.mappings.iter() {
            rules.push(CollectedRule {
                trigger,
                outputs: output_events.iter().collect(),
                apps: group.apps.clone(),
            });
        }
    }

    // Collect all keys used in triggers and outputs to find passthrough
    // candidates.
    let mut used_keys = std::collections::HashSet::new();
    for rule in &rules {
        used_keys.insert(rule.trigger.base);
        for mod_key in &rule.trigger.modifiers {
            used_keys.insert(*mod_key);
        }
        for output in &rule.outputs {
            used_keys.insert(output.base);
            for mod_key in &output.modifiers {
                used_keys.insert(*mod_key);
            }
        }
    }

    // Pick 5 passthrough keys that are not used by any rule.  Consumer
    // page usages are excluded: the macOS injector has no CGKeyCode for
    // them, so they cannot be part of a cross-platform injection sequence.
    let passthrough_keys: Vec<HidUsage> = HidUsage::all()
        .iter()
        .skip(9) // skip modifier keys and CapsLock
        .copied()
        .filter(|k| k.page() != PAGE_CONSUMER)
        .filter(|k| !used_keys.contains(k))
        .filter(|k| !platform_excludes_passthrough(*k))
        .take(5)
        .collect();

    if passthrough_keys.len() < 5 {
        panic!(
            "config uses too many unique keys to find 5 passthrough \
             candidates (used {} out of {})",
            used_keys.len(),
            HidUsage::all().len()
        );
    }

    // Build injection steps and expected events.  The sequence alternates
    // between triggers and passthrough keys to thoroughly exercise both
    // code paths.
    let mut steps: Vec<InjectionStep> = Vec::new();
    let mut expected: Vec<LogEvent> = Vec::new();

    let mut passthrough_iter = passthrough_keys.iter();

    // Interleave triggers with passthrough keys.  Insert a passthrough
    // after every two triggers to keep the test sequence manageable.
    let mut rule_idx = 0;
    let mut passthrough_count = 0;

    while rule_idx < rules.len() || passthrough_count < 5 {
        // Add up to 2 triggers before the next passthrough.
        let triggers_to_add = std::cmp::min(2, rules.len() - rule_idx);
        for _ in 0..triggers_to_add {
            let rule = &rules[rule_idx];

            // Build injection step for the trigger.
            steps.push(key_event_to_injection_step(rule.trigger));

            // Build expected events for the step.
            expected.extend(rule_expected_events(rule, &rules));

            rule_idx += 1;
        }

        // Add a passthrough key if we still have some left.
        if let Some(&passthrough_key) = passthrough_iter.next() {
            steps.push(single_key_injection_step(passthrough_key));

            // Passthrough keys are forwarded unchanged (subject to the
            // monitor's visibility).
            expected.extend(passthrough_expected(passthrough_key));

            passthrough_count += 1;
        }
    }

    TestSequences { steps, expected }
}

/// Whether the platform's input stack rewrites a bare press of this key
/// in a way the expected sequence cannot predict.
///
/// On Windows, AltGr-capable layouts (e.g. German) expand an injected
/// Right Alt press into a Ctrl+Right Alt key pair, and Alt presses can put
/// the system into menu mode, so both Alt keys are never used as bare
/// passthrough keys there.
#[cfg(target_os = "windows")]
fn platform_excludes_passthrough(key: HidUsage) -> bool {
    matches!(key, HidUsage::LeftAlt | HidUsage::RightAlt)
}

#[cfg(not(target_os = "windows"))]
fn platform_excludes_passthrough(_key: HidUsage) -> bool {
    false
}

/// The key name the monitor logs for a given key, or `None` when the
/// monitor cannot see the key on this platform.
///
/// On every supported platform the monitor captures the daemon's output
/// directly (uinput grab on Linux, tagged hook on Windows, IOKit seizure of
/// the virtual keyboard on macOS), so every emitted key is visible under
/// its exact name (left/right sides are distinguished, Super and CapsLock
/// included).
fn monitor_key_name(key: HidUsage) -> Option<&'static str> {
    Some(key.as_str())
}

/// Modifier bit position, shared with the daemon's bitmask layout (see
/// `HidUsage::hid_usage_to_modifier_bit`).  The daemon emits output modifiers
/// in ascending bit order.
fn modifier_bit(key: HidUsage) -> Option<u8> {
    HidUsage::hid_usage_to_modifier_bit(key)
}

/// Build the expected monitor events for a forwarded (passthrough) key
/// press+release.
fn passthrough_expected(key: HidUsage) -> Vec<LogEvent> {
    match monitor_key_name(key) {
        Some(name) => vec![event_str(name, true), event_str(name, false)],
        None => Vec::new(),
    }
}

/// Find a firing rule whose trigger is the bare modifier *mod_key* (no
/// modifiers held), if one exists.
fn find_bare_modifier_rule<'a>(
    mod_key: HidUsage,
    rules: &'a [CollectedRule<'a>],
) -> Option<&'a CollectedRule<'a>> {
    rules.iter().find(|rule| {
        rule.trigger.base == mod_key
            && rule.trigger.modifiers.is_empty()
            && rule.fires_for_monitor()
    })
}

/// Build the expected monitor events for one daemon-emitted output tap.
///
/// The daemon emits each output as a complete tap: modifiers down (bit
/// order), base down, base up, modifiers up (reverse).  Sub-events the
/// monitor cannot see on this platform (e.g. Super on Linux) are omitted.
fn output_tap_events(
    outputs: &[&keymapper::common::config::KeyEvent],
) -> Vec<LogEvent> {
    let mut events = Vec::new();

    for output in outputs {
        let mut mod_keys: Vec<HidUsage> = output.modifiers.clone();
        mod_keys.sort_by_key(|k| modifier_bit(*k).unwrap_or(8));

        for mod_key in &mod_keys {
            if let Some(name) = monitor_key_name(*mod_key) {
                events.push(event_str(name, true));
            }
        }

        if let Some(name) = monitor_key_name(output.base) {
            events.push(event_str(name, true));
            events.push(event_str(name, false));
        }

        for mod_key in mod_keys.iter().rev() {
            if let Some(name) = monitor_key_name(*mod_key) {
                events.push(event_str(name, false));
            }
        }
    }

    events
}

/// Build the expected monitor events for one trigger injection step.
///
/// Models the daemon's processing of the step's events in order: each
/// trigger modifier press/release is forwarded to the virtual device
/// (unless the bare modifier is itself a firing trigger, in which case the
/// press emits that rule's output and the release is swallowed), the base
/// press fires this rule (emitting each output as a complete tap, with the
/// base release swallowed) when the rule applies to the active app, or is
/// forwarded together with the release when the rule is scoped to another
/// app.
fn rule_expected_events<'a>(
    rule: &CollectedRule<'a>,
    rules: &'a [CollectedRule<'a>],
) -> Vec<LogEvent> {
    let mut events = Vec::new();
    let trigger = rule.trigger;

    // Modifier presses are processed before the base key.  A forwarded
    // (passthrough) modifier press emits only its down event here; the
    // matching up event is emitted in the release phase below, after the
    // base key, mirroring the daemon's forwarding order.
    for mod_key in &trigger.modifiers {
        if let Some(bare) = find_bare_modifier_rule(*mod_key, rules) {
            events.extend(output_tap_events(&bare.outputs));
        } else if let Some(name) = monitor_key_name(*mod_key) {
            events.push(event_str(name, true));
        }
    }

    if rule.fires_for_monitor() {
        // Base press fires the rule.  The trigger's modifiers were forwarded
        // when pressed; the daemon releases them on the virtual keyboard
        // before emitting the outputs, so expect their up events here.  The
        // base release is swallowed.
        for mod_key in trigger.modifiers.iter().rev() {
            if find_bare_modifier_rule(*mod_key, rules).is_none()
                && let Some(name) = monitor_key_name(*mod_key)
            {
                events.push(event_str(name, false));
            }
        }
        for output in &rule.outputs {
            events.extend(output_tap_events(std::slice::from_ref(output)));
        }
    } else {
        // Rule does not apply (scoped to another app): the whole step
        // passes through unchanged.
        events.extend(passthrough_expected(trigger.base));

        // Modifier releases are processed after the base key, in reverse
        // order.
        for mod_key in trigger.modifiers.iter().rev() {
            if find_bare_modifier_rule(*mod_key, rules).is_none()
                && let Some(name) = monitor_key_name(*mod_key)
            {
                events.push(event_str(name, false));
            }
            // A bare-modifier trigger swallows the release (no emission on
            // key-up), so nothing is expected for it here.
        }
    }

    events
}

/// Convert a `[KeyEvent]` into an injection step with properly ordered
/// modifier and base key presses.
fn key_event_to_injection_step(
    key_event: &keymapper::common::config::KeyEvent,
) -> InjectionStep {
    // Convert each HidUsage to its platform native code.
    let mut keys_down: Vec<u16> = key_event
        .modifiers
        .iter()
        .map(|k| common_to_platform_code(*k))
        .collect();
    let base_code = common_to_platform_code(key_event.base);
    keys_down.push(base_code);

    // Release in reverse order: base first, then modifiers last-to-first.
    let mut keys_up: Vec<u16> = vec![base_code];
    for mod_code in key_event.modifiers.iter().rev() {
        keys_up.push(common_to_platform_code(*mod_code));
    }

    InjectionStep { keys_down, keys_up }
}

/// Build an injection step for a single key with no modifiers.
fn single_key_injection_step(key: HidUsage) -> InjectionStep {
    let code = common_to_platform_code(key);
    InjectionStep {
        keys_down: vec![code],
        keys_up: vec![code],
    }
}

/// Convert a platform-agnostic `[HidUsage]` to the platform-specific
/// native key code for injection.
///
/// On macOS this returns CGKeyCodes for use with the CGEvent-based injector.
/// On other platforms it returns the platform-native key code.
#[cfg(target_os = "macos")]
fn common_to_platform_code(usage: HidUsage) -> u16 {
    // CGKeyCode lookup derived from USB HID Usage Tables.
    match usage {
        HidUsage::A => 0,
        HidUsage::S => 1,
        HidUsage::D => 2,
        HidUsage::F => 3,
        HidUsage::H => 4,
        HidUsage::G => 5,
        HidUsage::Z => 6,
        HidUsage::X => 7,
        HidUsage::C => 8,
        HidUsage::V => 9,
        HidUsage::IsoExtra => 10,
        HidUsage::B => 11,
        HidUsage::Q => 12,
        HidUsage::W => 13,
        HidUsage::E => 14,
        HidUsage::R => 15,
        HidUsage::Y => 16,
        HidUsage::T => 17,
        HidUsage::Number1 => 18,
        HidUsage::Number2 => 19,
        HidUsage::Number3 => 20,
        HidUsage::Number4 => 21,
        HidUsage::Number6 => 22,
        HidUsage::Number5 => 23,
        HidUsage::Equal => 24,
        HidUsage::Number9 => 25,
        HidUsage::Number7 => 26,
        HidUsage::Minus => 27,
        HidUsage::Number8 => 28,
        HidUsage::Number0 => 29,
        HidUsage::BracketRight => 30,
        HidUsage::O => 31,
        HidUsage::U => 32,
        HidUsage::BracketLeft => 33,
        HidUsage::I => 34,
        HidUsage::P => 35,
        HidUsage::Return => 36,
        HidUsage::L => 37,
        HidUsage::J => 38,
        HidUsage::Quote => 39,
        HidUsage::K => 40,
        HidUsage::Semicolon => 41,
        HidUsage::Backslash => 42,
        HidUsage::Comma => 43,
        HidUsage::Slash => 44,
        HidUsage::N => 45,
        HidUsage::M => 46,
        HidUsage::Period => 47,
        HidUsage::Tab => 48,
        HidUsage::Space => 49,
        HidUsage::Grave => 50,
        HidUsage::Backspace => 51,
        HidUsage::Escape => 53,
        HidUsage::Delete => 117,
        HidUsage::RightCommand => 54,
        HidUsage::LeftCommand => 55,
        HidUsage::LeftShift => 56,
        HidUsage::CapsLock => 57,
        HidUsage::LeftAlt => 58,
        HidUsage::LeftControl => 59,
        HidUsage::RightShift => 60,
        HidUsage::RightAlt => 61,
        HidUsage::RightControl => 62,
        HidUsage::NumpadDecimal => 65,
        HidUsage::NumpadPlus => 69,
        HidUsage::NumpadClear => 71,
        HidUsage::NumpadDivide => 73,
        HidUsage::NumpadMultiply => 75,
        HidUsage::NumpadEnter => 76,
        HidUsage::NumpadMinus => 78,
        HidUsage::NumpadEqual => 90,
        HidUsage::Numpad0 => 82,
        HidUsage::Numpad1 => 83,
        HidUsage::Numpad2 => 84,
        HidUsage::Numpad3 => 85,
        HidUsage::Numpad4 => 86,
        HidUsage::Numpad5 => 87,
        HidUsage::Numpad6 => 88,
        HidUsage::Numpad7 => 89,
        HidUsage::Numpad8 => 91,
        HidUsage::Numpad9 => 92,
        HidUsage::F5 => 96,
        HidUsage::F6 => 97,
        HidUsage::F7 => 98,
        HidUsage::F3 => 99,
        HidUsage::F8 => 100,
        HidUsage::F9 => 101,
        HidUsage::F11 => 103,
        HidUsage::F10 => 109,
        HidUsage::F12 => 111,
        HidUsage::Home => 115,
        HidUsage::PageUp => 116,
        HidUsage::F4 => 118,
        HidUsage::End => 119,
        HidUsage::F2 => 120,
        HidUsage::PageDown => 121,
        HidUsage::F1 => 122,
        HidUsage::LeftArrow => 123,
        HidUsage::RightArrow => 124,
        HidUsage::DownArrow => 125,
        HidUsage::UpArrow => 126,
        HidUsage::IsoHash => 50, // Non-US # and ~
        // Consumer page usages have no CGKeyCode and cannot be injected.
        other => {
            panic!("no CGKeyCode for consumer page usage {}", other.as_str())
        }
    }
}

/// Convert a platform-agnostic `[HidUsage]` to the evdev `KEY_*` code for
/// injection on Linux.
///
/// The injector emits real evdev key codes plus an `MSC_SCAN` carrying the
/// HID usage, mirroring a physical HID keyboard.  The daemon's static
/// translation table is the single source of truth for the mapping.
#[cfg(target_os = "linux")]
fn common_to_platform_code(usage: HidUsage) -> u16 {
    keymapper::platform::hid_translate::hid_usage_to_keycode(usage)
        .expect("every HidUsage has an evdev code")
}

/// Convert a platform-agnostic `[HidUsage]` to the Windows virtual-key
/// code for injection.
///
/// The daemon's low-level hook receives the injected VK code and converts
/// it to a `HidUsage` via the platform `Key` table, mirroring the physical
/// keyboard path.
#[cfg(windows)]
fn common_to_platform_code(usage: HidUsage) -> u16 {
    keymapper::platform::Key::from_hid_usage(usage)
        .expect("every HidUsage has a VK code on Windows")
        .as_native()
}

// ---------------------------------------------------------------------------
// Process management helpers
// ---------------------------------------------------------------------------

/// RAII guard that removes a temporary directory on `Drop`.
///
/// Declare this before the process guards, because guards drop in reverse
/// declaration order and the daemon's PID file (needed to stop it) lives
/// inside the directory.
struct TempDirGuard {
    path: Option<PathBuf>,
}

impl TempDirGuard {
    fn new(path: PathBuf) -> Self {
        TempDirGuard { path: Some(path) }
    }

    fn remove(&mut self) {
        if let Some(path) = self.path.take()
            && let Err(e) = std::fs::remove_dir_all(&path)
        {
            eprintln!("failed to remove temp dir {:?}: {e}", path);
        }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        self.remove();
    }
}

/// RAII guard that kills a child process on `Drop`.
struct ProcessGuard {
    child: Option<std::process::Child>,
    label: &'static str,
}

impl ProcessGuard {
    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            eprintln!("stopping {}...", self.label);
            child.kill().ok();
            let _ = child.wait();
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Kill any `keymapperd` processes orphaned by a previous, interrupted run.
///
/// The daemon runs in its own session (see `spawn_daemon`), so it survives
/// the death of the test process that started it: a Ctrl-C or nextest
/// cancellation kills the test before `DaemonGuard` can stop the daemon.
/// The orphan keeps holding the keyboard grab, and because each run uses a
/// fresh temp directory (and thus a fresh PID file), the next run cannot
/// see it.  Callers must hold the e2e lock, so no live e2e daemon can be
/// mistaken for a stale one.
#[cfg(unix)]
fn kill_stale_daemons() {
    // SIGKILL any orphaned daemons.
    kill_orphaned_daemons();

    // Wait for the killed daemons' virtual keyboard nodes to be destroyed,
    // so the monitor does not seize a stale node.  This runs even when no
    // daemon was found, because a node can outlive its daemon: the service
    // daemon destroys it asynchronously after the client socket closes.
    #[cfg(target_os = "macos")]
    wait_for_output_keyboard_gone();

    // Give the kernel a moment to release the seized devices.
    thread::sleep(Duration::from_millis(500));
}

/// SIGKILL any `keymapperd` processes found via `pgrep`.
#[cfg(unix)]
fn kill_orphaned_daemons() {
    let Ok(output) =
        Command::new("pgrep").arg("-x").arg("keymapperd").output()
    else {
        return;
    };
    // pgrep exits non-zero when nothing matches.
    if !output.status.success() {
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Ok(pid) = line.trim().parse::<i32>() {
            eprintln!(
                "killing stale keymapperd (pid {pid}) from a previous run"
            );
            // Safety: kill(2) with a pid read from pgrep's output.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

/// Wait until no Karabiner output keyboard node remains in IOKit.
///
/// A stale daemon's output keyboard (the Karabiner DriverKit virtual device)
/// is destroyed asynchronously by the service daemon when the daemon's client
/// socket closes (on SIGKILL).  If a node lingers when the e2e monitor starts,
/// the monitor may seize it; when it is later destroyed, the seizure is void
/// and the new daemon's output leaks into the focused window (e.g. running a
/// shell-history command in the user's terminal).  Polling until the node is
/// gone closes that race.
///
/// The manager is scheduled with the current run loop and pumped while
/// waiting, because `IOHIDManagerCopyDevices` only reflects devices the
/// manager has been notified about, and removal notifications are delivered
/// through the run loop.
#[cfg(target_os = "macos")]
fn wait_for_output_keyboard_gone() {
    use keymapper::platform::HidDeviceManager;
    use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};

    let Ok(manager) = HidDeviceManager::new_keyboard_matcher() else {
        eprintln!(
            "warning: could not create IOHIDManager; skipping the wait for \
             stale virtual keyboards"
        );
        return;
    };

    manager.schedule_with_runloop();

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if manager.find_karabiner_virtual_keyboard().is_none() {
            return;
        }

        if Instant::now() >= deadline {
            eprintln!(
                "warning: a stale Karabiner output keyboard node is still \
                 present after 10 s; the e2e monitor may seize it and miss \
                 the new daemon's output"
            );
            return;
        }

        // Pump the run loop so removal notifications are processed; a short
        // timeout keeps the deadline check responsive.
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.1, true);
    }
}

/// RAII guard that stops the daemon on `Drop`.
///
/// The daemon runs detached from the test process, so a panic in the test
/// would leave it running and holding the keyboard grab.  This guard runs
/// `keymapper daemon stop` on drop to make the cleanup unconditional.  It
/// only logs failures, because `stop` may run during stack unwinding,
/// where a panic would abort the process.
struct DaemonGuard {
    config_dir: PathBuf,
    stopped: bool,
}

impl DaemonGuard {
    /// Stop the daemon via `keymapper daemon stop`.  Idempotent.
    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;

        eprintln!("stopping daemon...");
        match Command::new(cli_bin_path())
            .args(["daemon", "stop", "--config-dir"])
            .arg(&self.config_dir)
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!("daemon stop failed with status: {status}")
            }
            Err(e) => eprintln!("failed to run daemon stop: {e}"),
        }

        // Allow the daemon to release its devices.
        thread::sleep(Duration::from_millis(200));
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn the monitor binary as a subprocess.  Returns a guard that ensures
/// the child is killed on `Drop`.
fn start_monitor(output_path: &Path) -> ProcessGuard {
    let child = Command::new(monitor_bin_path())
        .arg("--output")
        .arg(output_path)
        .spawn()
        .expect("failed to spawn keymapper_monitor");

    // Wrap the child in the guard before the readiness check, so the
    // monitor is still killed when the check panics.
    let mut guard = ProcessGuard {
        child: Some(child),
        label: "monitor",
    };

    // Give the monitor a moment to start up, so an immediate crash (e.g.
    // failing to open the output file) is caught by the liveness check
    // below.
    thread::sleep(Duration::from_secs(2));

    // Check that the monitor is still alive.  `try_wait` returns
    // `Ok(None)` while the process has not yet exited, so `None` means
    // "still running".
    let exited = guard
        .child
        .as_mut()
        .expect("child is present")
        .try_wait()
        .expect("failed to poll keymapper_monitor");
    if let Some(status) = exited {
        panic!("keymapper_monitor exited prematurely: {status}");
    }

    guard
}

/// Start the daemon via `keymapper daemon start --config-dir <path>`.
///
/// Returns a guard that stops the daemon on `Drop`, so the daemon is
/// cleaned up even when the test fails.
fn start_daemon(config_dir: &Path) -> DaemonGuard {
    // Pin the daemon's active-app query to the monitor's app name so
    // app-scoped rule evaluation is deterministic: the monitor is windowless
    // and can never be the compositor's active window, yet the test expects
    // the monitor-scoped rules to fire.
    //
    // Also tell the daemon where to write its readiness file so we can wait
    // for it to finish initialisation (e.g. the DriverKit virtual HID driver
    // loading on macOS) before injecting keys.  The daemon inherits this
    // environment variable when it is spawned.
    let ready_file = config_dir.join("keymapperd.ready");
    let _ = std::fs::remove_file(&ready_file);

    let status = Command::new(cli_bin_path())
        .args(["daemon", "start", "--config-dir"])
        .arg(config_dir)
        .env("KEYMAPPER_ACTIVE_APP", MONITOR_APP_NAME)
        .env("KEYMAPPER_READY_FILE", &ready_file)
        // On Windows, run the daemon in capture mode so it re-emits every
        // key through the virtual keyboard tagged for the monitor's hook.
        // On macOS and Linux the variable is ignored: the macOS daemon
        // seizes the injection keyboard like any other physical keyboard and
        // sees injected keys directly, and the Linux daemon always re-emits
        // every key.
        .env("KEYMAPPER_CAPTURE", "1")
        .status()
        .expect("failed to run keymapper daemon start");

    if !status.success() {
        panic!("keymapper daemon start failed with status: {}", status);
    }

    // Wait for the daemon to signal readiness (it touches the ready file
    // once it can process events).  This is more reliable than a fixed sleep
    // because initialisation time varies: on a fresh CI runner the DriverKit
    // driver may take several seconds to load into IOKit.
    wait_for_ready_file(&ready_file, config_dir);

    DaemonGuard {
        config_dir: config_dir.to_path_buf(),
        stopped: false,
    }
}

/// Wait until the daemon's readiness file appears, or fail.
///
/// Polls the ready file the daemon touches once it can process events.  If
/// the daemon exits before signalling readiness, or the wait times out, the
/// daemon log (which captures startup failures such as a missing DriverKit
/// driver) is printed before panicking.
fn wait_for_ready_file(ready_file: &Path, config_dir: &Path) {
    // Generous bound: the daemon's own driver-wait timeout is 15s, so it
    // will either signal readiness or exit well before this.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready_file.exists() {
        if !daemon_alive(config_dir) {
            eprintln!(
                "daemon exited before signalling readiness; log:\n{}",
                read_daemon_log(config_dir)
            );
            panic!("daemon exited before signalling readiness");
        }
        if Instant::now() >= deadline {
            eprintln!(
                "daemon did not signal readiness within 30s; log:\n{}",
                read_daemon_log(config_dir)
            );
            panic!("daemon did not signal readiness in time");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// Injector creation
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn create_injector() -> Result<Option<Box<dyn KeyInjector>>, InjectorError> {
    use keymapper::util::key_injector::MacOSInjector;
    let injector = MacOSInjector::new()?;
    Ok(injector.map(|i| Box::new(i) as Box<dyn KeyInjector>))
}

#[cfg(target_os = "linux")]
fn create_injector() -> Result<Option<Box<dyn KeyInjector>>, InjectorError> {
    use keymapper::util::key_injector::LinuxInjector;
    let injector = LinuxInjector::new()?;
    Ok(injector.map(|i| Box::new(i) as Box<dyn KeyInjector>))
}

#[cfg(target_os = "windows")]
fn create_injector() -> Result<Option<Box<dyn KeyInjector>>, InjectorError> {
    use keymapper::util::key_injector::WindowsInjector;
    let injector = WindowsInjector::new()?;
    Ok(injector.map(|i| Box::new(i) as Box<dyn KeyInjector>))
}

// ---------------------------------------------------------------------------
// Key injection helper
// ---------------------------------------------------------------------------

/// Inject a single step from the test sequence.  Presses all keys in
/// *step.keys_down* (down), then releases them in *step.keys_up* order.
fn inject_step(injector: &dyn KeyInjector, step: &InjectionStep) {
    for &code in &step.keys_down {
        injector
            .inject_key_down(code)
            .expect("failed to inject key down");
        // Small delay between key presses within a chord.
        thread::sleep(Duration::from_millis(3));
    }

    // Hold the full chord briefly so the down and up events are processed
    // as a distinct press+release pair.
    thread::sleep(Duration::from_millis(50));

    for &code in &step.keys_up {
        injector
            .inject_key_up(code)
            .expect("failed to inject key up");
        thread::sleep(Duration::from_millis(3));
    }

    // Delay between steps to let the daemon process events.
    thread::sleep(Duration::from_millis(50));
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Atomic counter for unique temp directory names.
static TEST_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Create a unique temporary directory for the test.
fn create_test_dir() -> PathBuf {
    let seq = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let dir = env::temp_dir().join(format!("keymapper_e2e_{pid}_{seq}"));
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

/// Wait until udev tags the injector's virtual device as a keyboard.
///
/// The daemon's startup discovery snapshots the udev database, and a
/// freshly created uinput device only appears as a keyboard once udevd has
/// processed its add event and tagged it with `ID_INPUT_KEYBOARD`.  Waiting
/// here makes the daemon's discovery deterministic; the daemon's post-listen
/// resync additionally covers the case where tagging still finishes late.
#[cfg(target_os = "linux")]
fn wait_for_injector_device(injector: &dyn KeyInjector) {
    let Some(path) = injector.input_device_path() else {
        eprintln!(
            "warning: injector reports no device path; skipping udev wait"
        );
        return;
    };

    use std::os::unix::fs::MetadataExt;

    for attempt in 0..100 {
        let tagged = std::fs::metadata(path)
            .ok()
            .and_then(|meta| {
                let device = udev::Device::from_devnum(
                    udev::DeviceType::Character,
                    meta.rdev(),
                )
                .ok()?;
                device
                    .property_value("ID_INPUT_KEYBOARD")
                    .map(|value| value == "1")
            })
            .unwrap_or(false);
        if tagged {
            return;
        }
        if attempt % 10 == 0 {
            eprintln!("waiting for udev to tag {path} as a keyboard...");
        }
        thread::sleep(Duration::from_millis(50));
    }

    eprintln!(
        "warning: udev did not tag {path} within 5s; the daemon's hot-plug \
         resync may grab it late"
    );
}

/// Wait until the injector's virtual keyboard appears in IOKit.
///
/// The daemon's startup discovery snapshots the connected keyboards, so the
/// injection keyboard (created by `injector.setup()`) must be registered in
/// IOKit before the daemon starts.  The manager is scheduled with the run
/// loop and pumped while waiting, because `IOHIDManagerCopyDevices` only
/// reflects devices the manager has been notified about.
#[cfg(target_os = "macos")]
fn wait_for_injector_device(_injector: &dyn KeyInjector) {
    use keymapper::platform::HidDeviceManager;
    use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};

    let manager = HidDeviceManager::new_keyboard_matcher()
        .expect("failed to create IOHIDManager");
    manager.schedule_with_runloop();

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let found = manager
            .scan_devices()
            .iter()
            .any(|device| device.is_injection_keyboard());
        if found {
            return;
        }

        if Instant::now() >= deadline {
            panic!(
                "the injection keyboard did not appear in IOKit within 15 s; \
                 is the DriverKit driver loaded?"
            );
        }

        // Pump the run loop so hotplug notifications are processed; a short
        // timeout keeps the deadline check responsive.
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.1, true);
    }
}

/// Check whether the daemon recorded in the config directory's PID file is
/// still alive.
#[cfg(unix)]
fn daemon_alive(config_dir: &Path) -> bool {
    let Ok(pid_str) =
        std::fs::read_to_string(config_dir.join("keymapperd.pid"))
    else {
        return false;
    };
    let Ok(pid) = pid_str.trim().parse::<u32>() else {
        return false;
    };
    // Safety: signal 0 is a pure liveness probe; no signal is delivered.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn daemon_alive(_config_dir: &Path) -> bool {
    true
}

/// Read the daemon's log file, if present.  The daemon's stdout/stderr are
/// redirected here (see `daemon_cmd`), so this surfaces startup failures such
/// as a missing DriverKit driver.  Read before the temp dir is cleaned up.
fn read_daemon_log(config_dir: &Path) -> String {
    let log_path = config_dir.join("keymapperd.log");
    std::fs::read_to_string(&log_path)
        .unwrap_or_else(|_| "<no daemon log found>".to_string())
}

// ---------------------------------------------------------------------------
// Integration tests
// ---------------------------------------------------------------------------

/// Write the config for one phase to *config_out*.
///
/// A `Some` fixture is copied verbatim.  A `None` phase writes an empty
/// config (`groups: []`) so the daemon starts with no rules, i.e. every key
/// passes through unchanged.  The daemon requires a config file to exist in
/// order to start, so even the "no config" mode writes one.
fn write_phase_config(config_out: &Path, phase: Option<&Path>) {
    match phase {
        Some(fixture) => {
            // Read + write instead of `fs::copy`: on Apple targets, copy
            // uses an APFS clone that inherits the fixture's ownership,
            // which breaks the daemon's config-ownership check when the
            // test runs as a different user (e.g. root via sudo).
            let content =
                std::fs::read(fixture).expect("failed to read config fixture");
            std::fs::write(config_out, content)
                .expect("failed to write config fixture");
        }
        None => {
            std::fs::write(config_out, "groups: []\n")
                .expect("failed to write empty config");
        }
    }
}

/// Run the full e2e test against a list of config fixtures.
///
/// An empty list runs a single passthrough-only phase (an empty config is
/// written so the daemon starts with no rules).  A single fixture runs one
/// phase.  Multiple fixtures run one phase each, with every fixture after the
/// first overwriting the previous one to provoke a hot-reload.
///
/// The daemon, monitor, and injector are started once and shared across all
/// phases; the combined expected sequence is the canary plus each phase's
/// expectations in order.
fn run_e2e(configs: &[&str], label: &str) {
    if !can_run_e2e() {
        eprintln!(
            "skipping e2e test: injector not available in this environment. \
             Set CI=1 and ensure required permissions are granted."
        );
        return;
    }

    // Serialize with the other e2e tests (system-wide input stack access).
    let _e2e_lock = E2eLock::acquire();

    // Kill any daemon orphaned by a previous, interrupted run (see
    // `kill_stale_daemons`).  Safe because we hold the e2e lock: no other
    // e2e test can be running, so any keymapperd we find is stale.
    #[cfg(unix)]
    kill_stale_daemons();

    // a. Create temp directory.  The guard removes it on drop, even when
    //    the test fails.  Declared first so it drops last: the daemon's
    //    PID file (needed to stop it) lives in this directory.
    let temp_dir = create_test_dir();
    let mut dir_guard = TempDirGuard::new(temp_dir.clone());
    eprintln!("test dir: {:?}", temp_dir);

    // a2. Truncate the Windows capture debug log so it contains only this
    //     run's trace.  The daemon appends to a fixed path outside the temp
    //     dir, which survives the temp dir's removal when the test fails.
    let _ = std::fs::remove_file(
        env::temp_dir().join("keymapper_capture_debug.log"),
    );

    // b. Resolve the phases.  Each phase is an optional fixture path; `None`
    //    means "no user config" (an empty config is written instead).
    let phases: Vec<Option<&Path>> = if configs.is_empty() {
        vec![None]
    } else {
        configs.iter().map(|&c| Some(Path::new(c))).collect()
    };

    // c. Write the first phase's config into the temp directory.
    let config_out = temp_dir.join("config.yaml");
    write_phase_config(&config_out, phases[0]);

    // d. Create events log path.
    let events_log = temp_dir.join("events.log");

    // e. Start the monitor.
    let mut monitor = start_monitor(&events_log);

    // f. Create and setup the injector.
    let mut injector = create_injector()
        .expect("failed to create injector")
        .expect("injector not available on this platform");
    injector.setup().expect("failed to setup injector");

    // f2. Wait until the injector device is fully registered, so the
    //     daemon's startup discovery sees it deterministically.
    #[cfg(target_os = "linux")]
    wait_for_injector_device(&*injector);
    #[cfg(target_os = "macos")]
    wait_for_injector_device(&*injector);

    // g. Start the daemon.  The guard stops it on drop, even when the test
    //    fails.
    let mut daemon = start_daemon(&temp_dir);

    // g2. Verify the daemon survived initialisation.
    if !daemon_alive(&temp_dir) {
        eprintln!("daemon log:\n{}", read_daemon_log(&temp_dir));
        panic!("daemon exited after startup");
    }

    // h. Inject a canary key first to verify the full capture path
    //    (injector -> daemon -> virtual keyboard -> monitor) is live before
    //    running the real sequence.
    eprintln!("injecting canary key (Space)...");
    inject_step(&*injector, &single_key_injection_step(HidUsage::Space));

    // i. Run each phase: inject its sequence and accumulate the expected
    //    events.  Phases after the first overwrite the config to provoke a
    //    hot-reload before injecting.
    let mut expected_combined = passthrough_expected(HidUsage::Space);
    for (i, phase) in phases.iter().enumerate() {
        if i > 0 {
            eprintln!(
                "hot-reloading config (phase {} of {})...",
                i + 1,
                phases.len()
            );
            write_phase_config(&config_out, *phase);
            // Wait for the daemon's reload debounce plus compilation time.
            thread::sleep(Duration::from_secs(2));
        }

        let sequences = build_test_sequences(*phase);
        eprintln!(
            "phase {}: injection steps: {}, expected events: {}",
            i + 1,
            sequences.steps.len(),
            sequences.expected.len()
        );
        for (j, step) in sequences.steps.iter().enumerate() {
            eprintln!(
                "  phase {} step {}: {:?} / {:?}",
                i + 1,
                j,
                step.keys_down,
                step.keys_up
            );
            inject_step(&*injector, step);
        }
        expected_combined.extend(sequences.expected.iter().cloned());
    }

    // j. Stop the daemon.
    daemon.stop();

    // k. Stop the monitor.
    monitor.kill();

    // l. Teardown the injector.
    injector.teardown();

    // m. Parse the event log.
    let actual = event_log::parse(&events_log).unwrap_or_else(|e| {
        panic!("failed to parse event log {:?}: {e}", events_log)
    });
    eprintln!("captured {} events from log", actual.len());

    if actual.is_empty() {
        // The temp dir (and thus the daemon log) is removed when the test
        // fails, so print it now while it is still available.
        eprintln!("daemon log:\n{}", read_daemon_log(&temp_dir));
        let debug_log = env::temp_dir().join("keymapper_capture_debug.log");
        if let Ok(contents) = std::fs::read_to_string(&debug_log) {
            eprintln!("capture debug log:\n{contents}");
        }
        panic!(
            "monitor captured no events at all — the capture path is not \
             live (check the monitor and daemon logs)"
        );
    }

    // n. Assert the event log matches the combined expected sequence (canary
    //    + all phases).
    if actual != expected_combined {
        // The temp dir (and thus the daemon log) is removed when the test
        // fails, so print both logs now while they are still available.
        eprintln!("daemon log:\n{}", read_daemon_log(&temp_dir));
        let debug_log = env::temp_dir().join("keymapper_capture_debug.log");
        if let Ok(contents) = std::fs::read_to_string(&debug_log) {
            eprintln!("capture debug log:\n{contents}");
        }
    }
    assert_events_match(
        &actual,
        &expected_combined,
        "event log does not match expected sequence",
    );

    // o. Clean up.
    dir_guard.remove();

    eprintln!("{label} PASSED");
}

/// Run the full e2e test against the comprehensive config fixture.
///
/// Parses the config to derive injection and expected sequences, then
/// validates that the daemon remaps keys correctly.
#[test]
fn e2e_comprehensive_config() {
    run_e2e(&[CONFIG_COMPREHENSIVE], "e2e_comprehensive_config");
}

/// Run the full e2e test with a hot-reload of the config.
///
/// Starts with `config_comprehensive.yaml`, then hot-reloads to
/// `config_reloaded.yaml` and validates the new mappings.
#[test]
fn e2e_config_hot_reload() {
    run_e2e(
        &[CONFIG_COMPREHENSIVE, CONFIG_RELOADED],
        "e2e_config_hot_reload",
    );
}

/// Run the e2e test with no user config: an empty config is written so the
/// daemon starts with no rules, and only passthrough keys are exercised.
#[test]
fn e2e_no_config() {
    run_e2e(&[], "e2e_no_config");
}
