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
//! These tests spawn `keymapper_monitor` as a subprocess to capture keyboard
//! events into a log file, inject synthetic key events via the platform
//! injector, and validate the daemon's remapped output against expected
//! sequences derived from the config fixture files.
//!
//! The test flow is:
//! 1. Create a temp directory and copy a fixture config into it.
//! 2. Parse the config to extract trigger keys and their mapped outputs from
//!    all rule groups (regardless of app/keyboard context).
//! 3. Build an injection sequence containing triggers plus passthrough keys.
//! 4. Build an expected sequence containing mapped outputs plus passthrough.
//! 5. Start the monitor, injector, and daemon.
//! 6. Inject keys from the sequence and assert the event log matches the
//!    expected sequence.
//!
//! The temp directory, the monitor process, and the daemon are wrapped in
//! RAII guards, so the environment is cleaned up even when a test fails.

mod event_log;

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use event_log::{LogEvent, assert_events_match, event_str};
use keymapper::{
    common::{
        config::AppConfig,
        hid_usage::{HidUsage, PAGE_CONSUMER},
    },
    util::key_injector::{InjectorError, KeyInjector},
};

// ---------------------------------------------------------------------------
// CI gate — e2e tests require elevated privileges and a clean environment
// ---------------------------------------------------------------------------

/// Cache the result of the e2e capability check.  We probe once at startup
/// rather than per-test, because the result is a global property of the CI
/// environment (Accessibility permission, HID driver availability, etc.).
static CAN_RUN_E2E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn can_run_e2e() -> bool {
    *CAN_RUN_E2E.get_or_init(|| {
        if !should_run_e2e_raw() {
            return false;
        }

        // Check that the injector can be created.
        if !create_injector().is_ok_and(|opt| opt.is_some()) {
            return false;
        }

        true
    })
}

/// Raw check: is the `CI` env-var set?
fn should_run_e2e_raw() -> bool {
    env::var("CI").is_ok()
}

/// Check whether e2e tests should run.
fn should_run_e2e() -> bool {
    can_run_e2e()
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
    /// Ordered injection steps: triggers followed by passthrough keys.
    steps: Vec<InjectionStep>,
    /// Expected log events corresponding to each injection step.
    expected: Vec<LogEvent>,
}

/// Parse the config file at *config_path* and build test sequences.
///
/// Extracts all trigger keys from every rule group (regardless of app or
/// keyboard context), and adds passthrough keys that are not used by any
/// rule.  The injection sequence interleaves mapped triggers with
/// passthrough keys to exercise both remapping and transparent forwarding.
fn build_test_sequences(config_path: &Path) -> TestSequences {
    let content = std::fs::read_to_string(config_path).unwrap_or_else(|e| {
        panic!("failed to read config fixture {config_path:?}: {e}")
    });

    let app_config = AppConfig::load_from_str(&content).unwrap_or_else(|e| {
        panic!("failed to parse config fixture {config_path:?}: {e}")
    });

    // Collect all triggers and their outputs from every rule group.
    let mut triggers: Vec<&keymapper::common::config::KeyEvent> = Vec::new();
    let mut outputs: Vec<Vec<keymapper::common::config::KeyEvent>> =
        Vec::new();

    for group in &app_config.groups {
        for (trigger, output_events) in group.mappings.iter() {
            triggers.push(trigger);
            outputs.push(output_events.to_vec());
        }
    }

    // Collect all keys used in triggers and outputs to find passthrough
    // candidates.
    let mut used_keys = std::collections::HashSet::new();
    for trigger in &triggers {
        used_keys.insert(trigger.base);
        for mod_key in &trigger.modifiers {
            used_keys.insert(*mod_key);
        }
    }
    for output_group in &outputs {
        for output in output_group {
            used_keys.insert(output.base);
            for mod_key in &output.modifiers {
                used_keys.insert(*mod_key);
            }
        }
    }

    // Pick 5 passthrough keys that are not used by any rule.  Consumer
    // page usages are excluded: the egui monitor cannot capture them and
    // the macOS injector has no CGKeyCode for them.
    let passthrough_keys: Vec<HidUsage> = HidUsage::all()
        .iter()
        .copied()
        .filter(|k| k.page() != PAGE_CONSUMER)
        .filter(|k| !used_keys.contains(k))
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
    // between mapped triggers and passthrough keys to thoroughly exercise
    // both code paths.
    let mut steps: Vec<InjectionStep> = Vec::new();
    let mut expected: Vec<LogEvent> = Vec::new();

    let mut passthrough_iter = passthrough_keys.iter();

    // Interleave triggers with passthrough keys.  Insert a passthrough
    // after every two triggers to keep the test sequence manageable.
    let mut trigger_idx = 0;
    let mut passthrough_count = 0;

    while trigger_idx < triggers.len() || passthrough_count < 5 {
        // Add up to 2 triggers before the next passthrough.
        let triggers_to_add = std::cmp::min(2, triggers.len() - trigger_idx);
        for _ in 0..triggers_to_add {
            let trigger = triggers[trigger_idx];
            let output_group = &outputs[trigger_idx];

            // Build injection step for the trigger.
            let step = key_event_to_injection_step(trigger);
            steps.push(step);

            // Build expected events for the mapped output.
            #[allow(clippy::needless_borrow)]
            let trigger_down_events =
                build_expected_output_events(&output_group, true);
            #[allow(clippy::needless_borrow)]
            let trigger_up_events =
                build_expected_output_events(&output_group, false);

            expected.extend(trigger_down_events);
            expected.extend(trigger_up_events);

            trigger_idx += 1;
        }

        // Add a passthrough key if we still have some left.
        if let Some(&passthrough_key) = passthrough_iter.next() {
            let step = single_key_injection_step(passthrough_key);
            steps.push(step);

            // Passthrough keys pass through unchanged.
            expected.push(event_str(passthrough_key.as_str(), true));
            expected.push(event_str(passthrough_key.as_str(), false));

            passthrough_count += 1;
        }
    }

    TestSequences { steps, expected }
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

/// Build the expected log events for a group of output KeyEvents.
///
/// When *is_down* is true, emits down events for each output key in order.
/// When false, emits up events in reverse order (matching the daemon's
/// chord output semantics).
fn build_expected_output_events(
    outputs: &[keymapper::common::config::KeyEvent],
    is_down: bool,
) -> Vec<LogEvent> {
    let mut events = Vec::new();

    let ordered_outputs = if is_down {
        outputs.to_vec()
    } else {
        outputs.iter().rev().cloned().collect()
    };

    for output in &ordered_outputs {
        // For a chord output, emit modifier downs first, then base.
        if is_down {
            for mod_key in &output.modifiers {
                events.push(event_str(mod_key.as_str(), true));
            }
            events.push(event_str(output.base.as_str(), true));
        } else {
            // Release base first, then modifiers in reverse.
            events.push(event_str(output.base.as_str(), false));
            for mod_key in output.modifiers.iter().rev() {
                events.push(event_str(mod_key.as_str(), false));
            }
        }
    }

    events
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

    // Give the egui window time to open and become ready.
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
    let status = Command::new(cli_bin_path())
        .args(["daemon", "start", "--config-dir"])
        .arg(config_dir)
        .status()
        .expect("failed to run keymapper daemon start");

    if !status.success() {
        panic!("keymapper daemon start failed with status: {}", status);
    }

    // Allow the daemon to initialize (grab devices, create uinput, etc.).
    thread::sleep(Duration::from_millis(500));

    DaemonGuard {
        config_dir: config_dir.to_path_buf(),
        stopped: false,
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

    // Brief hold time for the full chord.
    thread::sleep(Duration::from_millis(10));

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

// ---------------------------------------------------------------------------
// Integration tests
// ---------------------------------------------------------------------------

/// Run the full e2e test against the comprehensive config fixture.
///
/// Parses the config to derive injection and expected sequences, then
/// validates that the daemon remaps keys correctly.
#[test]
fn e2e_comprehensive_config() {
    if !should_run_e2e() {
        eprintln!(
            "skipping e2e test: injector not available in this environment. \
             Set CI=1 and ensure required permissions are granted."
        );
        return;
    }

    // a. Create temp directory.  The guard removes it on drop, even when
    //    the test fails.  Declared first so it drops last: the daemon's
    //    PID file (needed to stop it) lives in this directory.
    let temp_dir = create_test_dir();
    let mut dir_guard = TempDirGuard::new(temp_dir.clone());
    eprintln!("test dir: {:?}", temp_dir);

    // b. Copy fixture config into temp directory.
    let config_in = Path::new(CONFIG_COMPREHENSIVE);
    let config_out = temp_dir.join("config.yaml");
    std::fs::copy(config_in, &config_out)
        .expect("failed to copy config fixture");

    // c. Create events log path.
    let events_log = temp_dir.join("events.log");

    // Parse the config to build injection and expected sequences.
    let sequences = build_test_sequences(&config_out);
    eprintln!(
        "injection steps: {}, expected events: {}",
        sequences.steps.len(),
        sequences.expected.len()
    );

    // d. Start the monitor.
    let mut monitor = start_monitor(&events_log);

    // e. Create and setup the injector.
    let mut injector = create_injector()
        .expect("failed to create injector")
        .expect("injector not available on this platform");
    injector.setup().expect("failed to setup injector");

    // f. Start the daemon.  The guard stops it on drop, even when the test
    //    fails.
    let mut daemon = start_daemon(&temp_dir);

    // g. Inject keys from the test sequence.
    eprintln!("injecting {} steps...", sequences.steps.len());
    for (i, step) in sequences.steps.iter().enumerate() {
        eprintln!("  step {}: {:?} / {:?}", i, step.keys_down, step.keys_up);
        inject_step(&*injector, step);
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

    // n. Assert the event log matches the expected sequence.
    assert_events_match(
        &actual,
        &sequences.expected,
        "event log does not match expected sequence",
    );

    // o. Clean up.
    dir_guard.remove();

    eprintln!("e2e_comprehensive_config PASSED");
}

/// Run the full e2e test with a hot-reload of the config.
///
/// 1. Starts with `config_comprehensive.yaml` and injects keys.
/// 2. Hot-reloads to `config_reloaded.yaml`.
/// 3. Injects keys again and validates the new mappings.
#[test]
fn e2e_config_hot_reload() {
    if !should_run_e2e() {
        eprintln!(
            "skipping e2e test: injector not available in this environment. \
             Set CI=1 and ensure required permissions are granted."
        );
        return;
    }

    // a. Create temp directory.  The guard removes it on drop, even when
    //    the test fails.  Declared first so it drops last: the daemon's
    //    PID file (needed to stop it) lives in this directory.
    let temp_dir = create_test_dir();
    let mut dir_guard = TempDirGuard::new(temp_dir.clone());
    eprintln!("test dir: {:?}", temp_dir);

    // b. Copy initial fixture config into temp directory.
    let config_in = Path::new(CONFIG_COMPREHENSIVE);
    let config_out = temp_dir.join("config.yaml");
    std::fs::copy(config_in, &config_out)
        .expect("failed to copy config fixture");

    // c. Create events log path.
    let events_log = temp_dir.join("events.log");

    // Parse the initial config to build injection and expected sequences.
    let sequences_phase1 = build_test_sequences(&config_out);
    eprintln!(
        "phase 1: injection steps: {}, expected events: {}",
        sequences_phase1.steps.len(),
        sequences_phase1.expected.len()
    );

    // d. Start the monitor.
    let mut monitor = start_monitor(&events_log);

    // e. Create and setup the injector.
    let mut injector = create_injector()
        .expect("failed to create injector")
        .expect("injector not available on this platform");
    injector.setup().expect("failed to setup injector");

    // f. Start the daemon.  The guard stops it on drop, even when the test
    //    fails.
    let mut daemon = start_daemon(&temp_dir);

    // g. Inject keys from phase 1 sequence.
    eprintln!(
        "phase 1: injecting {} steps...",
        sequences_phase1.steps.len()
    );
    for step in &sequences_phase1.steps {
        inject_step(&*injector, step);
    }

    // h. Hot-reload: copy the reloaded config into the temp directory.
    eprintln!("hot-reloading config...");
    let reload_config = Path::new(CONFIG_RELOADED);
    std::fs::copy(reload_config, &config_out)
        .expect("failed to copy reloaded config");

    // Wait for the daemon's reload debounce plus compilation time.
    thread::sleep(Duration::from_secs(2));

    // Parse the reloaded config to build phase 2 sequences.
    let sequences_phase2 = build_test_sequences(&config_out);
    eprintln!(
        "phase 2: injection steps: {}, expected events: {}",
        sequences_phase2.steps.len(),
        sequences_phase2.expected.len()
    );

    // i. Inject keys from phase 2 sequence (under new config).
    eprintln!(
        "phase 2: injecting {} steps...",
        sequences_phase2.steps.len()
    );
    for step in &sequences_phase2.steps {
        inject_step(&*injector, step);
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

    // n. Build the combined expected sequence: phase 1 + phase 2.
    let mut expected_combined = sequences_phase1.expected.clone();
    expected_combined.extend(sequences_phase2.expected.iter().cloned());

    assert_events_match(
        &actual,
        &expected_combined,
        "event log does not match combined expected sequence (phase 1 + \
         phase 2 after hot-reload)",
    );

    // o. Clean up.
    dir_guard.remove();

    eprintln!("e2e_config_hot_reload PASSED");
}
