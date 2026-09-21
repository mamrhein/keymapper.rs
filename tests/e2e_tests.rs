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
//! starts `keymapperd` (a production build with no test features), and
//! focuses a "normal" application — `keymapper_reader`, an ordinary raw-mode
//! stdin reader with no capture machinery of its own — so the daemon's
//! output reaches it through the OS's regular input path, exactly as if a
//! user had typed the keys.  The harness injects each phase's key sequence
//! once, waits for the reader to record the expected number of bytes, and
//! compares them against the character-space translation of the config's
//! expected output events (see [`char_translate`]).
//!
//! Because the harness clobbers the real user config directory and injects
//! session-wide keys, it refuses to run outside a CI environment (see
//! [`in_ci`]).  The original config is backed up and restored on teardown.
//!
//! The test flow is:
//! 1. Acquire the cross-process e2e lock and kill any stale daemons.
//! 2. Plant the fixture config (global rules only; app-scoped groups are
//!    rejected by the sequence builder).
//! 3. Create and set up the key injector (its virtual device must exist before
//!    the daemon starts so the daemon grabs it at startup).
//! 4. Start the daemon (waits for its readiness line on stdout).
//! 5. Start the reader and wait until it has keyboard focus (its output file
//!    is created only after focus and raw mode are established).
//! 6. For each phase: record the reader's byte offset, inject the phase's
//!    sequence once, wait for the expected bytes (plus a quiescence check for
//!    unexpected extras), and compare; for later phases, hot-reload the config
//!    first.
//! 7. Teardown: stop the daemon and reader; restore the config.

mod char_translate;
mod common;
mod event_log;
mod log_verify;

use std::{
    env,
    io::BufRead,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use char_translate::{Platform, current_platform, events_to_bytes};
use common::E2eLock;
use event_log::{LogEvent, event_str};
use keymapper::{
    common::{config::AppConfig, hid_usage::HidUsage},
    daemon::engine::fmt_key_event,
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

/// The fixed passthrough keys: plain letters that no fixture rule uses, so
/// they are forwarded unchanged and compare as lowercase bytes on every
/// platform.
const PASSTHROUGH_KEYS: [HidUsage; 5] = [
    HidUsage::D,
    HidUsage::E,
    HidUsage::F,
    HidUsage::G,
    HidUsage::H,
];

/// The probe key for modifier→modifier chords: a plain letter that no fixture
/// rule uses, pressed while the remapped modifier is held so the remap
/// produces an observable byte (e.g. Ctrl+X = 0x18).
const CHORD_PROBE_KEY: HidUsage = HidUsage::X;

/// Represents a single injection step in the test sequence.  Each step injects
/// down events followed by up events with small delays between.
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
    /// Expected log events corresponding to each injection step.  Kept for
    /// the legacy byte comparison; removed in phase 4.
    expected: Vec<LogEvent>,
    /// The log-based expected model for the phase (emit sequence plus
    /// passthrough and injected key sets), verified against the daemon's log.
    model: log_verify::ExpectedPhase,
}

/// A rule collected from the config.  The harness supports global rules only
/// (first iteration); app-scoped groups are rejected by the builder.
struct CollectedRule<'a> {
    /// The trigger key event (base plus held modifiers).
    trigger: &'a keymapper::common::config::KeyEvent,
    /// The rule's output key events.
    outputs: Vec<&'a keymapper::common::config::KeyEvent>,
}

/// Build test sequences for one config phase.
///
/// *config_content* is the config YAML (global rules only).  All trigger
/// rules are collected, and the expected sequence simulates the daemon's
/// per-event behaviour.  Passthrough keys that no rule uses are interleaved
/// with the triggers to exercise both remapping and transparent forwarding.
fn build_test_sequences(config_content: &str) -> TestSequences {
    let app_config = AppConfig::load_from_str(config_content)
        .unwrap_or_else(|e| panic!("failed to parse config: {e}"));

    // Collect all rules from every group; app-scoped groups are rejected
    // because the harness cannot control which app is active.
    let mut rules: Vec<CollectedRule> = Vec::new();
    for group in &app_config.groups {
        assert!(
            group.apps.is_empty(),
            "app-scoped rules are not supported by the e2e harness (first \
             iteration); group {:?} is scoped to apps {:?}",
            group.name,
            group.apps
        );
        for (trigger, output_events) in group.mappings.iter() {
            rules.push(CollectedRule {
                trigger,
                outputs: output_events.iter().collect(),
            });
        }
    }

    // Collect all keys used in triggers and outputs.
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

    // The fixed passthrough keys and the chord probe key must not be used by
    // any rule; otherwise their expected bytes would be wrong.
    for key in PASSTHROUGH_KEYS {
        assert!(
            !used_keys.contains(&key),
            "passthrough key {} is used by a fixture rule; choose another \
             passthrough key",
            key.as_str()
        );
        assert!(
            is_injectable(key),
            "passthrough key {} cannot be injected on this platform",
            key.as_str()
        );
    }
    assert!(
        !used_keys.contains(&CHORD_PROBE_KEY),
        "chord probe key {} is used by a fixture rule; choose another probe \
         key",
        CHORD_PROBE_KEY.as_str()
    );

    // Build injection steps and expected events, alternating triggers and
    // passthrough keys.  The log-based model is built in parallel: each fired
    // rule contributes its outputs as rendered `NativeKey`s, and every key
    // that must pass through (trigger modifiers, the chord probe key, the
    // fixed passthrough keys) is collected for the presence check.
    let platform = current_platform();
    let mut steps: Vec<InjectionStep> = Vec::new();
    let mut expected: Vec<LogEvent> = Vec::new();
    let mut model = log_verify::ExpectedPhase {
        emits: Vec::new(),
        passthrough_keys: Vec::new(),
        injected_keys: Vec::new(),
    };

    let mut passthrough_iter = PASSTHROUGH_KEYS.iter();
    let mut rule_idx = 0;
    let mut passthrough_count = 0;

    while rule_idx < rules.len() || passthrough_count < PASSTHROUGH_KEYS.len()
    {
        let triggers_to_add = std::cmp::min(2, rules.len() - rule_idx);
        for _ in 0..triggers_to_add {
            let rule = &rules[rule_idx];
            if let Some((step, chord_events)) = modifier_chord_step(rule) {
                steps.push(step);
                expected.extend(chord_events);
                // The rule fires on the trigger's down, emitting its single
                // held output modifier; the probe key passes through.
                model.emits.push(fmt_key_event(rule.outputs[0]));
                model.passthrough_keys.push(CHORD_PROBE_KEY);
                model.injected_keys.push(rule.trigger.base);
                model.injected_keys.push(CHORD_PROBE_KEY);
            } else {
                steps.push(key_event_to_injection_step(rule.trigger));
                expected.extend(rule_expected_events(rule, &rules, platform));
                for output in &rule.outputs {
                    model.emits.push(fmt_key_event(output));
                }
                // The trigger's modifiers are forwarded to the application.
                model
                    .passthrough_keys
                    .extend(rule.trigger.modifiers.iter().copied());
                model
                    .injected_keys
                    .extend(rule.trigger.modifiers.iter().copied());
                model.injected_keys.push(rule.trigger.base);
            }
            rule_idx += 1;
        }

        if let Some(&passthrough_key) = passthrough_iter.next() {
            steps.push(single_key_injection_step(passthrough_key));
            expected.extend(passthrough_expected(passthrough_key));
            model.passthrough_keys.push(passthrough_key);
            model.injected_keys.push(passthrough_key);
            passthrough_count += 1;
        }
    }

    // A key can be both an emitted output and a passthrough trigger modifier
    // (e.g. LeftControl in the comprehensive fixture); dedupe so each key is
    // checked once, keeping first-seen order.
    model.passthrough_keys = dedup_usages(model.passthrough_keys);
    model.injected_keys = dedup_usages(model.injected_keys);

    TestSequences {
        steps,
        expected,
        model,
    }
}

/// Remove duplicate usages, keeping first-seen order.
fn dedup_usages(keys: Vec<HidUsage>) -> Vec<HidUsage> {
    let mut seen = std::collections::HashSet::new();
    keys.into_iter().filter(|key| seen.insert(*key)).collect()
}

/// If *rule* is a bare-modifier trigger whose single output is itself a bare
/// modifier, build the chord step that makes the remap observable in
/// character space: hold the trigger, tap the probe key while the remapped
/// modifier is held, then release.  A plain tap would produce no bytes (the
/// output modifier is pressed and released with nothing in between).
///
/// The expected events model the daemon's output: the rule fires on the
/// trigger's down (emitting the held output modifier), the probe passes
/// through with it held, and the trigger's release emits the modifier's up.
fn modifier_chord_step(
    rule: &CollectedRule,
) -> Option<(InjectionStep, Vec<LogEvent>)> {
    let trigger = rule.trigger;
    if !trigger.modifiers.is_empty() || rule.outputs.len() != 1 {
        return None;
    }
    let output = rule.outputs[0];
    if !output.modifiers.is_empty()
        || HidUsage::hid_usage_to_modifier_bit(output.base).is_none()
    {
        return None;
    }

    Some((
        InjectionStep {
            keys_down: vec![trigger.base, CHORD_PROBE_KEY],
            keys_up: vec![CHORD_PROBE_KEY, trigger.base],
        },
        vec![
            event_str(output.base.as_str(), true),
            event_str(CHORD_PROBE_KEY.as_str(), true),
            event_str(CHORD_PROBE_KEY.as_str(), false),
            event_str(output.base.as_str(), false),
        ],
    ))
}

/// Build the expected events for a forwarded (passthrough) key press+release.
fn passthrough_expected(key: HidUsage) -> Vec<LogEvent> {
    vec![
        event_str(key.as_str(), true),
        event_str(key.as_str(), false),
    ]
}

/// Find a firing rule whose trigger is the bare modifier *mod_key* (no
/// modifiers held), if one exists.
fn find_bare_modifier_rule<'a>(
    mod_key: HidUsage,
    rules: &'a [CollectedRule<'a>],
) -> Option<&'a CollectedRule<'a>> {
    rules.iter().find(|rule| {
        rule.trigger.base == mod_key && rule.trigger.modifiers.is_empty()
    })
}

/// Modifier bit position, shared with the daemon's bitmask layout.  The daemon
/// emits output modifiers in ascending bit order.
fn modifier_bit(key: HidUsage) -> Option<u8> {
    HidUsage::hid_usage_to_modifier_bit(key)
}

/// Build the expected events for one daemon-emitted output tap.
fn output_tap_events(
    outputs: &[&keymapper::common::config::KeyEvent],
) -> Vec<LogEvent> {
    let mut events = Vec::new();

    for output in outputs {
        let mut mod_keys: Vec<HidUsage> = output.modifiers.clone();
        mod_keys.sort_by_key(|k| modifier_bit(*k).unwrap_or(8));

        for mod_key in &mod_keys {
            events.push(event_str(mod_key.as_str(), true));
        }

        events.push(event_str(output.base.as_str(), true));
        events.push(event_str(output.base.as_str(), false));

        for mod_key in mod_keys.iter().rev() {
            events.push(event_str(mod_key.as_str(), false));
        }
    }

    events
}

/// Build the expected events for one trigger injection step.
///
/// On Linux and Windows the daemon releases the trigger's forwarded modifiers
/// before emitting the mapped output (a clean tap), so the modifier ups come
/// first.  On macOS the release mask is inert: the trigger's modifiers are
/// physical events that pass through the tap, and their release reaches the
/// application only when the injector releases them — after the output.
fn rule_expected_events(
    rule: &CollectedRule,
    rules: &[CollectedRule],
    platform: Platform,
) -> Vec<LogEvent> {
    let mut events = Vec::new();
    let trigger = rule.trigger;

    for mod_key in &trigger.modifiers {
        if let Some(bare) = find_bare_modifier_rule(*mod_key, rules) {
            events.extend(output_tap_events(&bare.outputs));
        } else {
            events.push(event_str(mod_key.as_str(), true));
        }
    }

    if platform == Platform::Macos {
        for output in &rule.outputs {
            events.extend(output_tap_events(std::slice::from_ref(output)));
        }
        for mod_key in trigger.modifiers.iter().rev() {
            if find_bare_modifier_rule(*mod_key, rules).is_none() {
                events.push(event_str(mod_key.as_str(), false));
            }
        }
    } else {
        for mod_key in trigger.modifiers.iter().rev() {
            if find_bare_modifier_rule(*mod_key, rules).is_none() {
                events.push(event_str(mod_key.as_str(), false));
            }
        }
        for output in &rule.outputs {
            events.extend(output_tap_events(std::slice::from_ref(output)));
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

/// Build the config content for a phase: the fixture as-is, or an empty
/// config when *fixture* is `None`.
fn phase_content(fixture: Option<&Path>) -> String {
    match fixture {
        Some(path) => std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!("failed to read config fixture {path:?}: {e}")
        }),
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
        let (ready_tx, ready_rx) = mpsc::channel::<()>();

        // Drain the daemon's stdout for its lifetime, echoing each line to
        // stderr and signaling readiness.  The daemon logs after the
        // readiness line (e.g. the Windows hook-install notice); if the
        // pipe's read end were closed here, those writes would fail — on
        // Windows the daemon's main thread panics on the broken pipe and
        // dies, silently disabling all remapping.
        thread::spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                eprintln!("daemon: {line}");
                if line.contains(
                    "Cross-platform runtime engines fully synchronized.",
                ) {
                    let _ = ready_tx.send(());
                }
            }
        });

        // Wait for the readiness line with a timeout.  Polling (rather than
        // blocking on recv) also catches a daemon that exits before it ever
        // signals readiness.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match ready_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(()) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    child.kill().ok();
                    panic!("daemon exited before signaling readiness");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        child.kill().ok();
                        panic!("daemon did not signal readiness within 30 s");
                    }
                }
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
// Reader (the "normal app" with keyboard focus)
// ---------------------------------------------------------------------------

/// RAII guard for the `keymapper_reader` child: an ordinary raw-mode stdin
/// reader that records whatever bytes the OS delivers to the focused
/// terminal.  The platform-specific spawn gives it keyboard focus (the Linux
/// VT foreground, the macOS Terminal window, or the Windows console
/// foreground); its output file is created only after focus and raw mode are
/// established, which the harness uses as its ready signal.
struct Reader {
    /// The reader process, when it is a direct child (Linux and Windows).
    /// On macOS the reader runs inside Terminal.app and is killed via its
    /// unique output path instead.
    child: Option<std::process::Child>,
    /// The file the reader appends recorded bytes to.
    output_path: PathBuf,
    /// Keeps the unique temp directory alive for the reader's lifetime.
    _tempdir: tempfile::TempDir,
}

impl Reader {
    /// Spawn the reader with keyboard focus and wait for its ready signal
    /// (the output file).
    fn spawn() -> Self {
        let tempdir = tempfile::tempdir().expect("failed to create temp dir");
        let output_path = tempdir.path().join("recorded.bin");

        #[cfg(target_os = "linux")]
        {
            // The daemon's uinput output is routed by the kernel to the
            // active VT; stop getty (so it cannot retake the foreground) and
            // make tty1 active before the reader opens it.
            stop_getty_tty1();
            switch_to_vt(1);
        }

        let child = spawn_reader_process(&output_path);
        wait_for_ready(&output_path, Duration::from_secs(15));

        Reader {
            child,
            output_path,
            _tempdir: tempdir,
        }
    }

    /// The number of bytes recorded so far.
    fn len(&self) -> u64 {
        std::fs::metadata(&self.output_path)
            .map(|meta| meta.len())
            .unwrap_or(0)
    }

    /// Read the bytes recorded in [*start*, *end*).
    fn read_range(&self, start: u64, end: u64) -> Vec<u8> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = fs_err::File::open(&self.output_path)
            .expect("failed to open reader output file");
        file.seek(SeekFrom::Start(start))
            .expect("failed to seek in reader output");
        let mut buf = vec![0u8; (end - start) as usize];
        file.read_exact(&mut buf)
            .expect("failed to read reader output");
        buf
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill().ok();
            let _ = child.wait();
        }
        #[cfg(target_os = "macos")]
        {
            // The reader runs inside Terminal.app (not our child); the
            // unique output path makes the match unambiguous.  The Terminal
            // window itself is left open (local-only test).
            let _ = Command::new("pkill")
                .arg("-f")
                .arg(self.output_path.as_os_str())
                .status();
        }
        #[cfg(target_os = "linux")]
        start_getty_tty1();
    }
}

/// Spawn the reader process for this platform.  Returns `None` when the
/// reader is not a direct child (macOS: it runs inside Terminal.app).
#[cfg(target_os = "linux")]
fn spawn_reader_process(output_path: &Path) -> Option<std::process::Child> {
    // The reader setsid()s and opens /dev/tty1 itself in its own main, so
    // the kernel assigns it the controlling terminal and the VT's foreground
    // process group (an inherited fd would not do that).
    let child = Command::new(bin_path("keymapper_reader"))
        .arg(output_path)
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn keymapper_reader");
    Some(child)
}

/// Spawn the reader in a Terminal.app window (its stdin is then the window's
/// pty) and bring Terminal to the foreground.  The reader is not a direct
/// child of the harness, so `None` is returned.
#[cfg(target_os = "macos")]
fn spawn_reader_process(output_path: &Path) -> Option<std::process::Child> {
    let reader_bin = bin_path("keymapper_reader");
    // Two levels of quoting: the shell command inside `do script`, then the
    // AppleScript string literal around it.
    let shell_cmd = format!(
        "\"{}\" \"{}\"",
        escape_for_shell(&reader_bin.to_string_lossy()),
        escape_for_shell(&output_path.to_string_lossy()),
    );
    let script = format!(
        "tell application \"Terminal\"\nactivate\ndo script \
         \"{script}\"\nend tell",
        script = escape_for_applescript(&shell_cmd),
    );

    let mut child = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn osascript");
    // osascript exits once Terminal has started the script.
    let _ = child.wait();

    None
}

/// Escape a path for embedding in a double-quoted shell word.
#[cfg(target_os = "macos")]
fn escape_for_shell(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape a string for embedding in an AppleScript string literal.
#[cfg(target_os = "macos")]
fn escape_for_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Spawn the reader in its own console window (`CREATE_NEW_CONSOLE`) and
/// bring that window to the foreground so it has keyboard focus.  The daemon
/// re-emits keys via `SendInput`, which the system delivers to the foreground
/// window — i.e. here.
#[cfg(target_os = "windows")]
fn spawn_reader_process(output_path: &Path) -> Option<std::process::Child> {
    use std::os::windows::process::CommandExt;

    use windows::Win32::{
        Foundation::HWND,
        System::Threading::{AttachThreadInput, GetCurrentThreadId},
        UI::WindowsAndMessaging::{
            EnumWindows, GetForegroundWindow, GetWindowThreadProcessId,
            SetForegroundWindow,
        },
    };

    const CREATE_NEW_CONSOLE: u32 = 0x00000010;

    let child = Command::new(bin_path("keymapper_reader"))
        .arg(output_path)
        .stderr(Stdio::inherit())
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .expect("failed to spawn keymapper_reader");

    // The foreground lock may block SetForegroundWindow, so attach to the
    // current foreground window's thread first (the standard workaround).
    let mut ctx = EnumCtx {
        pid: child.id(),
        target: HWND::default(),
    };
    unsafe {
        let foreground = GetForegroundWindow();
        let fg_thread = if foreground.is_invalid() {
            0
        } else {
            GetWindowThreadProcessId(foreground, None)
        };
        let our_thread = GetCurrentThreadId();
        if fg_thread != 0 && fg_thread != our_thread {
            let _ = AttachThreadInput(our_thread, fg_thread, true);
        }

        // Safety: the callback only reads window properties and stores the
        // HWND of the reader's console window in the context.
        let _ = EnumWindows(
            Some(find_reader_window),
            windows::Win32::Foundation::LPARAM(
                &mut ctx as *mut EnumCtx as isize,
            ),
        );

        if !ctx.target.is_invalid() {
            let _ = SetForegroundWindow(ctx.target);
        } else {
            eprintln!("warning: could not find the reader's console window");
        }

        if fg_thread != 0 && fg_thread != our_thread {
            let _ = AttachThreadInput(our_thread, fg_thread, false);
        }
    }

    Some(child)
}

/// Context for the `EnumWindows` callback (which cannot capture).
#[cfg(target_os = "windows")]
struct EnumCtx {
    /// The reader's process id.
    pid: u32,
    /// The reader's console window, once found.
    target: windows::Win32::Foundation::HWND,
}

/// `EnumWindows` callback: finds the console window of the reader's process.
///  Cannot capture, so the context is passed via *lparam*.
#[cfg(target_os = "windows")]
unsafe extern "system" fn find_reader_window(
    hwnd: windows::Win32::Foundation::HWND,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::core::BOOL {
    use windows::Win32::{
        Foundation::{FALSE, TRUE},
        UI::WindowsAndMessaging::GetWindowThreadProcessId,
    };
    // Safety: *lparam* is the context pointer passed by the caller, which
    // keeps it alive for the duration of the enumeration.
    let ctx = unsafe { &mut *(lparam.0 as *mut EnumCtx) };
    let mut window_pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut window_pid)) };
    if window_pid == ctx.pid {
        ctx.target = hwnd;
        FALSE // stop the enumeration.
    } else {
        TRUE
    }
}

/// Stop the getty service on tty1 so it cannot retake the VT's foreground
/// process group mid-test.  Best-effort: a failure is reported but not fatal
/// (the getty may already be stopped).
#[cfg(target_os = "linux")]
fn stop_getty_tty1() {
    match Command::new("systemctl")
        .args(["stop", "getty@tty1"])
        .output()
    {
        Ok(o) if o.status.success() => {}
        Ok(o) => eprintln!(
            "warning: systemctl stop getty@tty1 failed ({}): {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => eprintln!("warning: failed to run systemctl: {e}"),
    }
}

/// Restart the getty service on tty1 (best-effort cleanup).
#[cfg(target_os = "linux")]
fn start_getty_tty1() {
    let _ = Command::new("systemctl")
        .args(["start", "getty@tty1"])
        .output();
}

/// Switch the active console to VT *vt* so the daemon's uinput output (which
/// the kernel routes to the active VT) reaches tty1.  Done via ioctl on
/// /dev/console rather than the `chvt` binary, because the harness process
/// has no controlling terminal in CI.
#[cfg(target_os = "linux")]
fn switch_to_vt(vt: i32) {
    // KDVT_SWITCHTO from linux/kd.h; not in the libc crate.
    const KDVT_SWITCHTO: libc::c_ulong = 0x4B39;
    // Safety: open(2)/ioctl(2)/close(2) on /dev/console; the harness runs as
    // root in CI.
    unsafe {
        let fd = libc::open(c"/dev/console".as_ptr(), libc::O_RDWR);
        if fd < 0 {
            eprintln!(
                "warning: failed to open /dev/console: {}",
                std::io::Error::last_os_error()
            );
            return;
        }
        if libc::ioctl(fd, KDVT_SWITCHTO, vt) < 0 {
            eprintln!(
                "warning: KDVT_SWITCHTO to VT{vt} failed: {}",
                std::io::Error::last_os_error()
            );
        }
        libc::close(fd);
    }
}

/// Wait for the reader's ready signal: its output file, which it creates only
/// after keyboard focus and raw mode are established.
fn wait_for_ready(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        if Instant::now() >= deadline {
            panic!(
                "the reader did not create its output file {path:?} within \
                 {} s (keyboard focus or raw mode failed)",
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// Byte comparison
// ---------------------------------------------------------------------------

/// Format bytes as a hex string for failure messages.
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Assert that *actual* bytes match *expected* exactly, with a readable diff.
fn assert_bytes_match(actual: &[u8], expected: &[u8], message: &str) {
    if actual == expected {
        return;
    }
    panic!(
        "{message}\nactual   = [{hex_actual}] ({text_actual})\nexpected = \
         [{hex_expected}] ({text_expected})",
        hex_actual = hex(actual),
        text_actual = String::from_utf8_lossy(actual),
        hex_expected = hex(expected),
        text_expected = String::from_utf8_lossy(expected),
    );
}

/// Wait until the reader has recorded at least *expected_len* new bytes
/// beyond *offset*, verify that no unexpected extra bytes follow (a
/// quiescence check), and return the phase's byte window.
fn read_phase_bytes(
    reader: &Reader,
    offset: u64,
    expected_len: usize,
    timeout: Duration,
) -> Vec<u8> {
    let deadline = Instant::now() + timeout;
    loop {
        if (reader.len() - offset) as usize >= expected_len {
            break;
        }
        if Instant::now() >= deadline {
            let got = reader.read_range(offset, reader.len());
            panic!(
                "timed out waiting for {expected_len} bytes; got {} so far: \
                 [{}] ({})",
                got.len(),
                hex(&got),
                String::from_utf8_lossy(&got)
            );
        }
        thread::sleep(Duration::from_millis(50));
    }

    // Quiescence: give the daemon a moment to emit any (unexpected) extra
    // bytes, then fail if more than expected arrived — an extra byte here
    // would otherwise leak into the next phase's window.
    thread::sleep(Duration::from_millis(500));
    let len = reader.len();
    if len > offset + expected_len as u64 {
        let extra = reader.read_range(offset + expected_len as u64, len);
        panic!(
            "received {} unexpected extra byte(s) after the expected \
             sequence: [{}] ({})",
            extra.len(),
            hex(&extra),
            String::from_utf8_lossy(&extra)
        );
    }

    reader.read_range(offset, offset + expected_len as u64)
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
/// config before injecting their sequence.
fn run_e2e(phases: &[Option<&Path>], label: &str) {
    if !require_ci(label) {
        return;
    }

    let _lock = E2eLock::acquire();
    kill_orphaned_daemons();

    // 1. Plant the initial config (global rules only).
    let initial_content = phase_content(phases[0]);
    let config = ConfigGuard::plant(&initial_content);

    // 2. Create and set up the injector (its virtual device must exist before
    //    the daemon starts so the daemon grabs it at startup).
    let mut injector = create_injector()
        .expect("failed to create injector")
        .expect("injector is available on this platform");
    injector.setup().expect("failed to set up injector");
    wait_for_injector_device(&*injector);

    // 3. Start the daemon (waits for its readiness line).
    let config_dir = keymapper::common::config_path::default_config_path()
        .expect("no default config path")
        .parent()
        .unwrap()
        .to_path_buf();
    let mut daemon = DaemonChild::spawn(&config_dir);

    // Give the daemon a moment to finish `start_mapping` (install its hook /
    // create its output device) before the reader takes focus.
    thread::sleep(Duration::from_millis(500));

    // 4. Start the reader (the "normal app" with keyboard focus) and wait for
    //    its ready signal.
    let reader = Reader::spawn();

    // 5. For each phase: inject the sequence once and compare the recorded
    //    bytes against the character-space translation of the expected events.
    //    Later phases hot-reload the config first.
    for (i, phase) in phases.iter().enumerate() {
        let content = phase_content(*phase);
        let sequences = build_test_sequences(&content);
        let expected_bytes =
            events_to_bytes(&sequences.expected, current_platform());

        if i > 0 {
            eprintln!(
                "hot-reloading config (phase {} of {})...",
                i + 1,
                phases.len()
            );
            config.overwrite(&content);
            // Wait for the daemon's reload debounce plus compilation time.
            thread::sleep(Duration::from_secs(2));
        }

        let offset = reader.len();
        eprintln!(
            "phase {}: injecting {} steps, expecting {} bytes...",
            i + 1,
            sequences.steps.len(),
            expected_bytes.len()
        );
        for step in &sequences.steps {
            inject_step(&*injector, step);
        }

        let actual = read_phase_bytes(
            &reader,
            offset,
            expected_bytes.len(),
            Duration::from_secs(15),
        );
        assert_bytes_match(
            &actual,
            &expected_bytes,
            "recorded bytes do not match the expected sequence",
        );
    }

    // 6. Teardown: stop the daemon; the reader (and its getty restart) and the
    //    config are restored via their Drop impls.
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

// ---------------------------------------------------------------------------
// Unit tests for the expected-model construction (no daemon needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// The comprehensive fixture: a modifier→modifier chord, a single-key
    /// remap, a chord output, and a modifier trigger.  The emit sequence must
    /// follow the config's document order, and the passthrough set must cover
    /// the chord probe key, the trigger modifier, and the fixed keys.
    #[test]
    fn build_test_sequences_comprehensive_model() {
        let content = std::fs::read_to_string(CONFIG_COMPREHENSIVE)
            .expect("failed to read comprehensive fixture");
        let sequences = build_test_sequences(&content);

        assert_eq!(
            sequences.model.emits,
            vec!["LeftControl", "B", "LeftCommand+A", "C"]
        );

        // LeftControl is both an emitted output (the chord) and a passthrough
        // trigger modifier; the dedupe keeps it exactly once.
        assert_eq!(
            sequences
                .model
                .passthrough_keys
                .iter()
                .copied()
                .collect::<HashSet<_>>(),
            HashSet::from([
                HidUsage::X,
                HidUsage::D,
                HidUsage::LeftControl,
                HidUsage::E,
                HidUsage::F,
                HidUsage::G,
                HidUsage::H,
            ])
        );

        assert_eq!(
            sequences
                .model
                .injected_keys
                .iter()
                .copied()
                .collect::<HashSet<_>>(),
            HashSet::from([
                HidUsage::CapsLock,
                HidUsage::X,
                HidUsage::A,
                HidUsage::D,
                HidUsage::Escape,
                HidUsage::LeftControl,
                HidUsage::Semicolon,
                HidUsage::E,
                HidUsage::F,
                HidUsage::G,
                HidUsage::H,
            ])
        );
    }

    /// A minimal inline config: one bare remap and one modifier trigger.  The
    /// trigger's base key is consumed (never passthrough), its modifier is.
    #[test]
    fn build_test_sequences_mini_config_model() {
        let content =
            "- name: \"mini\"\n  mappings:\n    A: B\n    Ctrl+C: V\n";
        let sequences = build_test_sequences(content);

        assert_eq!(sequences.model.emits, vec!["B", "V"]);
        assert_eq!(
            sequences.model.passthrough_keys,
            vec![
                HidUsage::LeftControl,
                HidUsage::D,
                HidUsage::E,
                HidUsage::F,
                HidUsage::G,
                HidUsage::H,
            ]
        );
        assert_eq!(
            sequences.model.injected_keys,
            vec![
                HidUsage::A,
                HidUsage::LeftControl,
                HidUsage::C,
                HidUsage::D,
                HidUsage::E,
                HidUsage::F,
                HidUsage::G,
                HidUsage::H,
            ]
        );
    }

    /// An empty config: no emits, and only the fixed passthrough keys are
    /// injected and expected to pass through.
    #[test]
    fn build_test_sequences_empty_config_model() {
        let sequences = build_test_sequences("groups: []");

        assert!(sequences.model.emits.is_empty());
        assert_eq!(
            sequences.model.passthrough_keys,
            vec![
                HidUsage::D,
                HidUsage::E,
                HidUsage::F,
                HidUsage::G,
                HidUsage::H,
            ]
        );
        assert_eq!(
            sequences.model.injected_keys,
            sequences.model.passthrough_keys
        );
    }
}
