// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! End-to-end integration tests that drive a *production* daemon.
//!
//! The harness plants a fixture config in the real user config directory,
//! starts `keymapperd` (a production build with no test features), focuses a
//! known test window so the daemon's active-app query is deterministic, and
//! captures the daemon's output through a live pipe from `keymapper_monitor
//! --stdout`.  A dedicated injector thread repeatedly injects a `Ctrl+Esc`
//! round delimiter followed by the config's trigger and passthrough keys; the
//! reader segments the captured stream on `Ctrl+Esc` and compares each round
//! against the expected output derived from the config.
//!
//! Because the harness clobbers the real user config directory and injects
//! session-wide keys, it refuses to run outside a CI environment (see
//! [`in_ci`]).  The original config is backed up and restored on teardown.
//!
//! The test flow is:
//! 1. Acquire the cross-process e2e lock and kill any stale daemons.
//! 2. Focus the test window and query the live active-app name.
//! 3. Plant the fixture config (substituting the app-name placeholder).
//! 4. Create and set up the key injector (its virtual device must exist before
//!    the daemon starts so the daemon grabs it at startup).
//! 5. Start the daemon (waits for its readiness line on stdout).
//! 6. Start the monitor (`--stdout`, piped) with a reader thread.
//! 7. Spawn the injector thread (rounds every 5 s).
//! 8. For each phase: read a matching round and compare; for later phases,
//!    hot-reload the config and swap the injector's key set first.
//! 9. Teardown: stop the injector, monitor, and daemon; restore the config.

mod common;
mod event_log;

use std::{
    env,
    io::BufRead,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use common::E2eLock;
use event_log::{LogEvent, assert_events_match, event_str, parse_line};
use keymapper::{
    common::{app_identity, config::AppConfig, hid_usage::HidUsage},
    test_util::key_injector::{InjectorError, KeyInjector, is_injectable},
};

// ---------------------------------------------------------------------------
// CI gate — e2e tests clobber the real config dir and inject session-wide keys
// ---------------------------------------------------------------------------

/// Whether the test is running in a CI environment.
///
/// The harness overwrites the real user config directory and injects
/// session-wide key events, so it must never run on an interactive machine.
/// A real CI system sets `CI` (or `GITHUB_ACTIONS`) to a non-empty value;
/// an empty value is treated as "not in CI".
fn in_ci() -> bool {
    env::var("CI").is_ok_and(|v| !v.is_empty())
        || env::var("GITHUB_ACTIONS").is_ok_and(|v| !v.is_empty())
}

/// Refuse to run outside CI.  Prints a clear error and returns `false` so the
/// caller can skip; no destructive action has been taken at this point.
fn require_ci(label: &str) -> bool {
    if in_ci() {
        return true;
    }
    eprintln!(
        "error: {label} requires a CI environment (set CI or \
         GITHUB_ACTIONS); refusing to clobber the local config dir and \
         inject session-wide keys"
    );
    false
}

// ---------------------------------------------------------------------------
// Test fixture paths
// ---------------------------------------------------------------------------

/// Path to the comprehensive config fixture.  Contains mappings that exercise
/// single-key remaps, chord outputs, and modifier triggers.
const CONFIG_COMPREHENSIVE: &str =
    "tests/fixtures/configs/config_comprehensive.yaml";

/// Path to the reloaded config fixture.  Contains different mappings to verify
/// hot-reload behavior.
const CONFIG_RELOADED: &str = "tests/fixtures/configs/config_reloaded.yaml";

/// Placeholder in the config fixtures that the harness replaces with the live
/// active-app name (the focused test window's resolved identity) before
/// planting, so the app-scoped rule fires deterministically on every platform.
const APP_PLACEHOLDER: &str = "__TEST_APP__";

// ---------------------------------------------------------------------------
// Binary path resolution
// ---------------------------------------------------------------------------

/// Resolve the path to a compiled binary that sits next to the test
/// executable.
fn bin_path(name: &str) -> PathBuf {
    env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(name)
}

// ---------------------------------------------------------------------------
// Config-driven sequence builders
// ---------------------------------------------------------------------------

/// Represents a single injection step in the test sequence.  Each step injects
/// a down event followed by an up event with a small delay between.
#[derive(Debug, Clone)]
struct InjectionStep {
    /// The usages to press (modifiers first, then base key).
    keys_down: Vec<HidUsage>,
    /// The usages to release (base key first, then modifiers).
    keys_up: Vec<HidUsage>,
}

/// The result of parsing a config for test sequence generation.
struct TestSequences {
    /// Ordered injection steps: triggers interleaved with passthrough keys.
    steps: Vec<InjectionStep>,
    /// Expected log events corresponding to each injection step.
    expected: Vec<LogEvent>,
}

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
    /// Whether the rule is expected to fire while *active_app* is the active
    /// application.
    fn fires_for_app(&self, active_app: &str) -> bool {
        self.apps.is_empty() || self.apps.iter().any(|a| a == active_app)
    }
}

/// Build test sequences for one config phase.
///
/// *config_content* is the (already app-name-substituted) config YAML.  All
/// trigger rules are collected (keeping app scope), and the expected sequence
/// simulates the daemon's per-event behaviour against *active_app*.
///
/// Passthrough keys that no rule uses are interleaved with the triggers to
/// exercise both remapping and transparent forwarding.
fn build_test_sequences(
    config_content: &str,
    active_app: &str,
) -> TestSequences {
    let app_config = AppConfig::load_from_str(config_content)
        .unwrap_or_else(|e| panic!("failed to parse config: {e}"));

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

    // Every trigger and output key in the fixture must be injectable on this
    // platform; an explicit assertion gives a clearer failure message.
    for key in &used_keys {
        assert!(
            is_injectable(*key),
            "fixture key {} cannot be injected on this platform",
            key.as_str()
        );
    }

    // Pick 5 passthrough keys that are not used by any rule and that the
    // platform injector can actually inject.
    let passthrough_keys: Vec<HidUsage> = HidUsage::all()
        .iter()
        .skip(9) // skip modifier keys and CapsLock
        .copied()
        .filter(|k| is_injectable(*k))
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

    // Build injection steps and expected events, alternating triggers and
    // passthrough keys.
    let mut steps: Vec<InjectionStep> = Vec::new();
    let mut expected: Vec<LogEvent> = Vec::new();

    let mut passthrough_iter = passthrough_keys.iter();
    let mut rule_idx = 0;
    let mut passthrough_count = 0;

    while rule_idx < rules.len() || passthrough_count < 5 {
        let triggers_to_add = std::cmp::min(2, rules.len() - rule_idx);
        for _ in 0..triggers_to_add {
            let rule = &rules[rule_idx];
            steps.push(key_event_to_injection_step(rule.trigger));
            expected.extend(rule_expected_events(rule, &rules, active_app));
            rule_idx += 1;
        }

        if let Some(&passthrough_key) = passthrough_iter.next() {
            steps.push(single_key_injection_step(passthrough_key));
            expected.extend(passthrough_expected(passthrough_key));
            passthrough_count += 1;
        }
    }

    TestSequences { steps, expected }
}

/// Whether the platform's input stack rewrites a bare press of this key in a
/// way the expected sequence cannot predict.
#[cfg(target_os = "windows")]
fn platform_excludes_passthrough(key: HidUsage) -> bool {
    matches!(key, HidUsage::LeftAlt | HidUsage::RightAlt)
}

#[cfg(not(target_os = "windows"))]
fn platform_excludes_passthrough(_key: HidUsage) -> bool {
    false
}

/// The key name the monitor logs for a given key, or `None` when the monitor
/// cannot see the key on this platform.  On every supported platform the
/// monitor captures the daemon's output directly, so every emitted key is
/// visible under its exact name.
fn monitor_key_name(key: HidUsage) -> Option<&'static str> {
    Some(key.as_str())
}

/// Modifier bit position, shared with the daemon's bitmask layout.  The daemon
/// emits output modifiers in ascending bit order.
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
    active_app: &str,
) -> Option<&'a CollectedRule<'a>> {
    rules.iter().find(|rule| {
        rule.trigger.base == mod_key
            && rule.trigger.modifiers.is_empty()
            && rule.fires_for_app(active_app)
    })
}

/// Build the expected monitor events for one daemon-emitted output tap.
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
fn rule_expected_events<'a>(
    rule: &CollectedRule<'a>,
    rules: &'a [CollectedRule<'a>],
    active_app: &str,
) -> Vec<LogEvent> {
    let mut events = Vec::new();
    let trigger = rule.trigger;

    for mod_key in &trigger.modifiers {
        if let Some(bare) =
            find_bare_modifier_rule(*mod_key, rules, active_app)
        {
            events.extend(output_tap_events(&bare.outputs));
        } else if let Some(name) = monitor_key_name(*mod_key) {
            events.push(event_str(name, true));
        }
    }

    if rule.fires_for_app(active_app) {
        for mod_key in trigger.modifiers.iter().rev() {
            if find_bare_modifier_rule(*mod_key, rules, active_app).is_none()
                && let Some(name) = monitor_key_name(*mod_key)
            {
                events.push(event_str(name, false));
            }
        }
        for output in &rule.outputs {
            events.extend(output_tap_events(std::slice::from_ref(output)));
        }
    } else {
        // Rule does not apply (scoped to another app): the whole step passes
        // through unchanged.
        events.extend(passthrough_expected(trigger.base));

        for mod_key in trigger.modifiers.iter().rev() {
            if find_bare_modifier_rule(*mod_key, rules, active_app).is_none()
                && let Some(name) = monitor_key_name(*mod_key)
            {
                events.push(event_str(name, false));
            }
        }
    }

    events
}

/// Convert a `[KeyEvent]` into an injection step with properly ordered
/// modifier and base key presses.
fn key_event_to_injection_step(
    key_event: &keymapper::common::config::KeyEvent,
) -> InjectionStep {
    let mut keys_down = key_event.modifiers.clone();
    keys_down.push(key_event.base);

    let mut keys_up = vec![key_event.base];
    for mod_key in key_event.modifiers.iter().rev() {
        keys_up.push(*mod_key);
    }

    InjectionStep { keys_down, keys_up }
}

/// Build an injection step for a single key with no modifiers.
fn single_key_injection_step(key: HidUsage) -> InjectionStep {
    InjectionStep {
        keys_down: vec![key],
        keys_up: vec![key],
    }
}

/// Inject one step (down events, brief hold, up events) with small delays so
/// each press+release is processed as a distinct pair.
fn inject_step(injector: &dyn KeyInjector, step: &InjectionStep) {
    for &usage in &step.keys_down {
        injector
            .inject_key_down(usage)
            .expect("failed to inject key down");
        thread::sleep(Duration::from_millis(3));
    }

    // Hold the full chord briefly so the down and up events are processed as a
    // distinct press+release pair.
    thread::sleep(Duration::from_millis(20));

    for &usage in &step.keys_up {
        injector
            .inject_key_up(usage)
            .expect("failed to inject key up");
        thread::sleep(Duration::from_millis(3));
    }

    // Brief pause between steps.
    thread::sleep(Duration::from_millis(30));
}

// ---------------------------------------------------------------------------
// The Ctrl+Esc round delimiter
// ---------------------------------------------------------------------------

/// The `Ctrl+Esc` round delimiter as an injection step (LeftControl + Escape).
fn ctrl_esc_injection_step() -> InjectionStep {
    InjectionStep {
        keys_down: vec![HidUsage::LeftControl, HidUsage::Escape],
        keys_up: vec![HidUsage::Escape, HidUsage::LeftControl],
    }
}

/// The `Ctrl+Esc` round delimiter as captured events.  The daemon forwards it
/// unchanged (no rule maps it), so it appears in the monitor's stream as these
/// four events and serves as a round boundary.
fn ctrl_esc_delimiter() -> Vec<LogEvent> {
    vec![
        event_str("LeftControl", true),
        event_str("Escape", true),
        event_str("Escape", false),
        event_str("LeftControl", false),
    ]
}

// ---------------------------------------------------------------------------
// Config planting (with backup/restore)
// ---------------------------------------------------------------------------

/// RAII guard that plants a config in the real user config directory and
/// restores the original on drop.
struct ConfigGuard {
    path: PathBuf,
    /// The original file bytes, or `None` if no config existed.
    backup: Option<Vec<u8>>,
}

impl ConfigGuard {
    /// Plant *content* at the default config path, backing up any existing
    /// file.
    fn plant(content: &str) -> Self {
        let path = keymapper::common::config_path::default_config_path()
            .expect("no default config path");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .expect("failed to create config directory");
        }

        let backup = std::fs::read(&path).ok();
        std::fs::write(&path, content).expect("failed to plant config");

        ConfigGuard { path, backup }
    }

    /// Overwrite the planted config (used to provoke a hot-reload).
    fn overwrite(&self, content: &str) {
        std::fs::write(&self.path, content)
            .expect("failed to overwrite config");
    }
}

impl Drop for ConfigGuard {
    fn drop(&mut self) {
        match &self.backup {
            Some(bytes) => {
                let _ = std::fs::write(&self.path, bytes);
            }
            None => {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// Build the config content for a phase: the fixture with the app-name
/// placeholder substituted, or an empty config when *fixture* is `None`.
fn phase_content(fixture: Option<&Path>, active_app: &str) -> String {
    match fixture {
        Some(path) => std::fs::read_to_string(path)
            .unwrap_or_else(|e| {
                panic!("failed to read config fixture {path:?}: {e}")
            })
            .replace(APP_PLACEHOLDER, active_app),
        None => "groups: []".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Daemon management (directly spawned, readiness line on stdout)
// ---------------------------------------------------------------------------

/// RAII guard for a directly-spawned `keymapperd` child.
struct DaemonChild {
    child: Option<std::process::Child>,
}

impl DaemonChild {
    /// Spawn `keymapperd` with CWD = the config directory and piped stdout,
    /// then wait for its readiness line.
    fn spawn(config_dir: &Path) -> Self {
        let mut child = Command::new(bin_path("keymapperd"))
            .current_dir(config_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("failed to spawn keymapperd");

        let stdout = child.stdout.take().expect("stdout is piped");
        let reader = std::io::BufReader::new(stdout);
        let deadline = Instant::now() + Duration::from_secs(30);

        for line in reader.lines() {
            let Ok(line) = line else { break };
            eprintln!("daemon: {line}");
            if line
                .contains("Cross-platform runtime engines fully synchronized.")
            {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().ok();
                panic!("daemon did not signal readiness within 30 s");
            }
        }

        DaemonChild { child: Some(child) }
    }

    /// Stop the daemon: SIGTERM (unix) with a grace period, then SIGKILL.
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            eprintln!("stopping daemon...");
            #[cfg(unix)]
            {
                // Safety: kill(2) on a PID we own (our child).
                unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
                for _ in 0..50 {
                    if child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
            child.kill().ok();
            let _ = child.wait();
        }
    }
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// Test window (deterministic active app)
// ---------------------------------------------------------------------------

/// RAII guard for the `keymapper_testwindow` child.
struct TestWindowChild {
    child: Option<std::process::Child>,
}

impl TestWindowChild {
    fn spawn() -> Self {
        let child = Command::new(bin_path("keymapper_testwindow"))
            .stderr(Stdio::inherit())
            .spawn()
            .expect("failed to spawn keymapper_testwindow");
        TestWindowChild { child: Some(child) }
    }
}

impl Drop for TestWindowChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill().ok();
            let _ = child.wait();
        }
    }
}

/// Install the test-window `.desktop` fixture so the daemon's app-id
/// resolution maps the helper's executable name to a known app id (Linux
/// only).  Must run before the daemon's first active-app query, because the
/// daemon's `.desktop` cache is built lazily on first use.
#[cfg(target_os = "linux")]
fn install_desktop_fixture() {
    let src =
        Path::new("tests/fixtures/applications/keymapper.testwindow.desktop");
    let dest_dir = dirs::home_dir()
        .expect("no home directory")
        .join(".local/share/applications");
    std::fs::create_dir_all(&dest_dir)
        .expect("failed to create applications dir");
    let dest = dest_dir.join("keymapper.testwindow.desktop");
    std::fs::copy(src, &dest).expect("failed to install .desktop fixture");
}

#[cfg(not(target_os = "linux"))]
fn install_desktop_fixture() {}

/// Query the live active-app name (the focused test window's resolved
/// identity), retrying briefly to let focus settle.
fn query_active_app() -> String {
    for _ in 0..10 {
        let name = app_identity::get_active_app_name();
        if !name.is_empty() && name != "unknown" {
            return name;
        }
        thread::sleep(Duration::from_millis(300));
    }
    panic!("could not resolve the test window's active app name");
}

// ---------------------------------------------------------------------------
// Monitor (piped stdout + reader thread)
// ---------------------------------------------------------------------------

/// The monitor child plus a channel of parsed events fed by a reader thread.
struct Monitor {
    child: std::process::Child,
    rx: mpsc::Receiver<LogEvent>,
}

impl Monitor {
    /// Spawn `keymapper_monitor --stdout` with piped stdout and a reader
    /// thread that parses lines into events.
    fn spawn() -> Self {
        let mut child = Command::new(bin_path("keymapper_monitor"))
            .arg("--stdout")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("failed to spawn keymapper_monitor");

        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if let Some(event) = parse_line(&line)
                            && tx.send(event).is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Monitor { child, rx }
    }

    fn kill(&mut self) {
        self.child.kill().ok();
        let _ = self.child.wait();
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Read events from the channel until a complete round (delimited by
/// `Ctrl+Esc`) is captured, returning the round's events (excluding the
/// delimiters).  Returns whatever was collected on timeout.
fn read_round(
    rx: &mpsc::Receiver<LogEvent>,
    timeout: Duration,
) -> Vec<LogEvent> {
    let delimiter = ctrl_esc_delimiter();
    let deadline = Instant::now() + timeout;
    let mut round: Vec<LogEvent> = Vec::new();
    let mut recent: Vec<LogEvent> = Vec::new();
    let mut in_round = false;

    loop {
        if Instant::now() >= deadline {
            eprintln!(
                "warning: timed out waiting for a complete round ({} events \
                 so far)",
                round.len()
            );
            return round;
        }

        let Ok(event) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };

        recent.push(event.clone());
        if recent.len() > 4 {
            recent.remove(0);
        }
        let is_delim = recent == delimiter;

        if !in_round {
            if is_delim {
                in_round = true;
                round.clear();
            }
        } else if is_delim {
            // The last 3 events already in `round` plus this one form the end
            // delimiter; drop them and return the completed round.
            for _ in 0..3 {
                round.pop();
            }
            return round;
        } else {
            round.push(event);
        }
    }
}

/// Read rounds until one matches *expected*, returning it.  Rounds that do not
/// match (e.g. produced before a hot-reload swapped the key set) are skipped.
/// Panics on timeout.
fn read_round_matching(
    rx: &mpsc::Receiver<LogEvent>,
    expected: &[LogEvent],
    timeout: Duration,
) -> Vec<LogEvent> {
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for a round matching the expected \
                 sequence ({} events)",
                expected.len()
            );
        }
        let round = read_round(rx, Duration::from_secs(15));
        if round == expected {
            return round;
        }
        eprintln!(
            "round did not match expected ({} vs {} events); reading next \
             round",
            round.len(),
            expected.len()
        );
    }
}

// ---------------------------------------------------------------------------
// Injector thread
// ---------------------------------------------------------------------------

/// Shared state for the injector thread: the current key set (swappable for
/// hot-reload) and a stop flag.
struct InjectorState {
    steps: Mutex<Vec<InjectionStep>>,
    stop: AtomicBool,
}

/// Spawn the injector thread.  It loops: sleep 5 s, inject the `Ctrl+Esc`
/// delimiter, then inject the current key set.  The key set is read from the
/// shared state each round so a hot-reload can swap it.
fn spawn_injector(
    injector: Box<dyn KeyInjector + Send>,
    state: Arc<InjectorState>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let delimiter = ctrl_esc_injection_step();
        loop {
            // Sleep in small increments so the stop flag is checked promptly.
            for _ in 0..50 {
                if state.stop.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            if state.stop.load(Ordering::Relaxed) {
                return;
            }

            inject_step(&*injector, &delimiter);

            let steps = state.steps.lock().unwrap().clone();
            for step in &steps {
                if state.stop.load(Ordering::Relaxed) {
                    return;
                }
                inject_step(&*injector, step);
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Stale-daemon cleanup
// ---------------------------------------------------------------------------

/// SIGKILL any `keymapperd` processes orphaned by a previous, interrupted
/// run.  Callers must hold the e2e lock so no live e2e daemon is mistaken for
/// a stale one.
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

#[cfg(not(unix))]
fn kill_orphaned_daemons() {}

// ---------------------------------------------------------------------------
// Injector creation
// ---------------------------------------------------------------------------

/// Create the platform key injector.  Returns `None` if the platform is
/// fundamentally unsupported, `Err` if runtime prerequisites are unmet.
fn create_injector()
-> Result<Option<Box<dyn KeyInjector + Send>>, InjectorError> {
    #[cfg(target_os = "macos")]
    {
        use keymapper::test_util::key_injector::MacOSInjector;
        let injector = MacOSInjector::new()?;
        Ok(injector.map(|i| Box::new(i) as Box<dyn KeyInjector + Send>))
    }
    #[cfg(target_os = "linux")]
    {
        use keymapper::test_util::key_injector::LinuxInjector;
        let injector = LinuxInjector::new()?;
        Ok(injector.map(|i| Box::new(i) as Box<dyn KeyInjector + Send>))
    }
    #[cfg(target_os = "windows")]
    {
        use keymapper::test_util::key_injector::WindowsInjector;
        let injector = WindowsInjector::new()?;
        Ok(injector.map(|i| Box::new(i) as Box<dyn KeyInjector + Send>))
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows"
    )))]
    {
        Err(InjectorError::NotSupported("platform not supported".into()))
    }
}

/// Wait until udev tags the injector's virtual device as a keyboard (Linux).
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
        "warning: udev did not tag {path} within 5 s; the daemon's hot-plug \
         resync may grab it late"
    );
}

#[cfg(not(target_os = "linux"))]
fn wait_for_injector_device(_injector: &dyn KeyInjector) {}

// ---------------------------------------------------------------------------
// Main orchestration
// ---------------------------------------------------------------------------

/// Run the e2e test for the given config phases.  Each phase is a fixture path
/// (or `None` for an empty config).  Phases after the first hot-reload the
/// config and swap the injector's key set before capturing their round.
fn run_e2e(phases: &[Option<&Path>], label: &str) {
    if !require_ci(label) {
        return;
    }

    let _lock = E2eLock::acquire();
    kill_orphaned_daemons();

    // 1. Focus the test window and query the live active-app name.
    let _window = TestWindowChild::spawn();
    let active_app = query_active_app();
    eprintln!("active app: {active_app}");

    // 2. Plant the initial config and install the app-id fixture (Linux).
    let initial_content = phase_content(phases[0], &active_app);
    let config = ConfigGuard::plant(&initial_content);
    install_desktop_fixture();

    // 3. Create and set up the injector (its virtual device must exist before
    //    the daemon starts so the daemon grabs it at startup).
    let mut injector = create_injector()
        .expect("failed to create injector")
        .expect("injector is available on this platform");
    injector.setup().expect("failed to set up injector");
    wait_for_injector_device(&*injector);

    // 4. Start the daemon (waits for its readiness line).
    let config_dir = keymapper::common::config_path::default_config_path()
        .expect("no default config path")
        .parent()
        .unwrap()
        .to_path_buf();
    let mut daemon = DaemonChild::spawn(&config_dir);

    // Give the daemon a moment to finish `start_mapping` (install its hook /
    // create its output device) before the monitor starts, so on Windows the
    // daemon's hook is installed first.
    thread::sleep(Duration::from_millis(500));

    // 5. Start the monitor (piped stdout + reader thread).
    let mut monitor = Monitor::spawn();

    // 6. Build the initial key set and spawn the injector thread.
    let initial_sequences =
        build_test_sequences(&initial_content, &active_app);
    eprintln!(
        "phase 1: injection steps: {}, expected events: {}",
        initial_sequences.steps.len(),
        initial_sequences.expected.len()
    );
    let state = Arc::new(InjectorState {
        steps: Mutex::new(initial_sequences.steps.clone()),
        stop: AtomicBool::new(false),
    });
    let injector_handle = spawn_injector(injector, state.clone());

    // 7. For each phase: read a matching round and compare.  Later phases
    //    hot-reload the config and swap the injector's key set first.
    for (i, phase) in phases.iter().enumerate() {
        if i > 0 {
            eprintln!(
                "hot-reloading config (phase {} of {})...",
                i + 1,
                phases.len()
            );
            let new_content = phase_content(*phase, &active_app);
            config.overwrite(&new_content);
            // Wait for the daemon's reload debounce plus compilation time.
            thread::sleep(Duration::from_secs(2));

            let new_sequences =
                build_test_sequences(&new_content, &active_app);
            *state.steps.lock().unwrap() = new_sequences.steps;
        }

        let expected = build_test_sequences(
            &phase_content(*phase, &active_app),
            &active_app,
        )
        .expected;

        eprintln!(
            "phase {}: waiting for a matching round ({} expected events)...",
            i + 1,
            expected.len()
        );
        let actual = read_round_matching(
            &monitor.rx,
            &expected,
            Duration::from_secs(60),
        );
        assert_events_match(
            &actual,
            &expected,
            "round does not match expected sequence",
        );
    }

    // 8. Teardown: stop the injector, monitor, and daemon; restore the config
    //    (via ConfigGuard's Drop) and kill the test window (via its Drop).
    state.stop.store(true, Ordering::Relaxed);
    let _ = injector_handle.join();
    monitor.kill();
    daemon.stop();

    eprintln!("{label} PASSED");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Run the e2e test with no user config: an empty config is planted so the
/// daemon starts with no rules, and only passthrough keys are exercised.
#[test]
fn e2e_no_config() {
    run_e2e(&[None], "e2e_no_config");
}

/// Run the full e2e test against the comprehensive config fixture.
#[test]
fn e2e_comprehensive_config() {
    run_e2e(
        &[Some(Path::new(CONFIG_COMPREHENSIVE))],
        "e2e_comprehensive_config",
    );
}

/// Run the full e2e test with a hot-reload of the config.
#[test]
fn e2e_config_hot_reload() {
    run_e2e(
        &[
            Some(Path::new(CONFIG_COMPREHENSIVE)),
            Some(Path::new(CONFIG_RELOADED)),
        ],
        "e2e_config_hot_reload",
    );
}
