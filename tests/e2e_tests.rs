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
//! verifies the daemon's decisions from its own debug log: before each phase
//! it raises the daemon's log level to `debug` (via the control socket),
//! injects the phase's key sequence, collects the log window until the
//! expected emits appear and the stream goes quiescent, resets the level to
//! `info`, and checks the window against the expected model derived from the
//! config (see [`log_verify`]): the `emit` sequence must match exactly, every
//! key that must pass through must appear in a `recv` and a `pass` line, and
//! no `ERROR` lines may occur.
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
//! 4. Start the daemon and wait for its readiness line in the log stream (the
//!    daemon's stderr, redirected to a temp file, on Linux; the daemon's
//!    rotating log file elsewhere).
//! 5. For each phase: hot-reload the config first (later phases) and wait for
//!    the daemon's hot-swap line, raise the log level to `debug`, inject the
//!    phase's sequence once, collect and verify the log window, and reset the
//!    level.
//! 6. Teardown: stop the daemon; restore the config.

mod common;
mod log_capture;
mod log_verify;

use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use common::E2eLock;
use keymapper::{
    common::{config::AppConfig, hid_usage::HidUsage},
    daemon::engine::fmt_key_event,
    test_util::key_injector::{InjectorError, KeyInjector, is_injectable},
};
use log_capture::{
    FileLogSource, LogSource, Mark, reset_to_default, set_debug,
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
/// they are forwarded unchanged.
const PASSTHROUGH_KEYS: [HidUsage; 5] = [
    HidUsage::D,
    HidUsage::E,
    HidUsage::F,
    HidUsage::G,
    HidUsage::H,
];

/// The probe key for modifier→modifier chords: a plain letter that no fixture
/// rule uses, pressed while the remapped modifier is held so the chord
/// produces an observable emit.
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
    /// The expected model for the phase (emit sequence plus passthrough and
    /// injected key sets), verified against the daemon's log.
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

    // Build injection steps, alternating triggers and passthrough keys.  The
    // expected model is built in parallel: each fired rule contributes its
    // outputs as rendered `NativeKey`s, and every key that must pass through
    // (trigger modifiers, the chord probe key, the fixed passthrough keys) is
    // collected for the presence check.
    let mut steps: Vec<InjectionStep> = Vec::new();
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
            if let Some(step) = modifier_chord_step(rule) {
                steps.push(step);
                // The rule fires on the trigger's down, emitting its single
                // held output modifier; the probe key passes through.
                model.emits.push(fmt_key_event(rule.outputs[0]));
                model.passthrough_keys.push(CHORD_PROBE_KEY);
                model.injected_keys.push(rule.trigger.base);
                model.injected_keys.push(CHORD_PROBE_KEY);
            } else {
                steps.push(key_event_to_injection_step(rule.trigger));
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

    TestSequences { steps, model }
}

/// Remove duplicate usages, keeping first-seen order.
fn dedup_usages(keys: Vec<HidUsage>) -> Vec<HidUsage> {
    let mut seen = std::collections::HashSet::new();
    keys.into_iter().filter(|key| seen.insert(*key)).collect()
}

/// If *rule* is a bare-modifier trigger whose single output is itself a bare
/// modifier, build the chord step that makes the remap observable: hold the
/// trigger, tap the probe key while the remapped modifier is held, then
/// release.  A plain tap would emit nothing observable (the output modifier
/// is pressed and released with nothing in between).
fn modifier_chord_step(rule: &CollectedRule) -> Option<InjectionStep> {
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

    Some(InjectionStep {
        keys_down: vec![trigger.base, CHORD_PROBE_KEY],
        keys_up: vec![CHORD_PROBE_KEY, trigger.base],
    })
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
// Daemon management (directly spawned, readiness line in the log stream)
// ---------------------------------------------------------------------------

/// RAII guard for a directly-spawned `keymapperd` child.
struct DaemonChild {
    /// The daemon process, until it is stopped.
    child: Option<std::process::Child>,
    /// The exit status, once the daemon has exited (cached by `poll_exit`).
    exit_status: Option<std::process::ExitStatus>,
    /// The file the daemon's stderr (its log stream) is redirected to.
    #[cfg(target_os = "linux")]
    stderr_path: PathBuf,
    /// Keeps the temp directory alive for the daemon's lifetime.
    #[cfg(target_os = "linux")]
    _stderr_tempdir: tempfile::TempDir,
}

impl DaemonChild {
    /// Spawn `keymapperd` with CWD = the config directory.  On Linux the
    /// daemon's stderr (its log stream) is redirected to a temp file the
    /// harness tails; on the other platforms the daemon logs to its rotating
    /// file and stderr is inherited (it carries only fallback notices).
    fn spawn(config_dir: &Path) -> Self {
        #[cfg(target_os = "linux")]
        let (stderr_path, stderr_tempdir) = {
            let tempdir =
                tempfile::tempdir().expect("failed to create temp dir");
            let path = tempdir.path().join("daemon-stderr.log");
            (path, tempdir)
        };

        let mut cmd = Command::new(bin_path("keymapperd"));
        cmd.current_dir(config_dir);
        // The daemon writes nothing to stdout (its log goes to stderr or a
        // file); null it rather than leaving a pipe that would fill up.
        cmd.stdout(Stdio::null());
        #[cfg(target_os = "linux")]
        {
            let file = fs_err::File::create(&stderr_path)
                .expect("failed to create the daemon's stderr file");
            cmd.stderr(Stdio::from(file));
        }
        #[cfg(not(target_os = "linux"))]
        {
            cmd.stderr(Stdio::inherit());
        }

        let child = cmd.spawn().expect("failed to spawn keymapperd");

        #[cfg(target_os = "linux")]
        {
            Self {
                child: Some(child),
                exit_status: None,
                stderr_path,
                _stderr_tempdir: stderr_tempdir,
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Self {
                child: Some(child),
                exit_status: None,
            }
        }
    }

    /// The log source for the daemon's log stream: the stderr temp file on
    /// Linux, the rotating log file elsewhere.
    fn log_source(&self) -> Box<dyn LogSource> {
        #[cfg(target_os = "linux")]
        {
            Box::new(FileLogSource::fixed(self.stderr_path.clone()))
        }
        #[cfg(target_os = "windows")]
        {
            let dir = dirs::data_local_dir()
                .expect("no local data directory (LOCALAPPDATA) available")
                .join("keymapperd")
                .join("logs");
            Box::new(FileLogSource::rotated(dir, "keymapperd"))
        }
        #[cfg(target_os = "macos")]
        {
            let dir = dirs::home_dir()
                .expect("no home directory available")
                .join("Library")
                .join("Logs")
                .join("keymapper");
            Box::new(FileLogSource::rotated(dir, "keymapperd"))
        }
    }

    /// Check whether the daemon has exited, caching its status.  Returns
    /// `true` once it has.
    fn poll_exit(&mut self) -> bool {
        if self.exit_status.is_some() {
            return true;
        }
        let Some(child) = self.child.as_mut() else {
            return true;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                self.exit_status = Some(status);
                true
            }
            Ok(None) => false,
            Err(e) => panic!("failed to poll the daemon: {e}"),
        }
    }

    /// Stop the daemon: SIGTERM (unix) with a grace period, then SIGKILL.
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            eprintln!("stopping daemon...");
            if self.exit_status.is_none() {
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
// Log-stream waits (readiness, hot-reload, phase windows)
// ---------------------------------------------------------------------------

/// The daemon's readiness line (INFO): emitted once the device grab, the
/// config watcher, and the engine sync are all done.
const READINESS_LINE: &str =
    "Cross-platform runtime engines fully synchronized.";

/// The daemon's hot-reload line (INFO): emitted once a config change has been
/// compiled and swapped in.
const HOT_SWAP_LINE: &str = "Configuration hot-swapped successfully!";

/// Poll *source* for a line containing *needle* until one appears or the
/// timeout elapses.  Fails fast when the daemon exits, but only after
/// draining what it wrote, so a line flushed just before exit is not missed.
fn wait_for_line(
    source: &mut Box<dyn LogSource>,
    daemon: &mut DaemonChild,
    mark: Mark,
    needle: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let lines = source.read_new(mark).map_err(|e| e.to_string())?;
        if lines.iter().any(|line| line.contains(needle)) {
            return Ok(());
        }
        if daemon.poll_exit() {
            return Err("the daemon exited before the expected log line \
                        appeared"
                .to_string());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out after {} s waiting for the log line {needle:?}",
                timeout.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Wait for the daemon's readiness line in its log stream.
fn wait_for_readiness(
    source: &mut Box<dyn LogSource>,
    daemon: &mut DaemonChild,
) {
    let mark = source.mark().expect("failed to mark the log stream");
    wait_for_line(
        source,
        daemon,
        mark,
        READINESS_LINE,
        Duration::from_secs(30),
    )
    .unwrap_or_else(|e| panic!("the daemon did not become ready: {e}"));
}

/// Collect the phase's log window: poll until at least *expected_emits* emit
/// lines have appeared, then keep reading until the stream is quiescent (no
/// new lines for 500 ms) so unexpected extras are captured in the window
/// instead of leaking into the next phase.
fn collect_window(
    source: &mut Box<dyn LogSource>,
    daemon: &mut DaemonChild,
    mark: Mark,
    expected_emits: &[String],
) -> Result<Vec<String>, String> {
    let mut window: Vec<String> = Vec::new();
    let mut emit_count = 0usize;

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let lines = source.read_new(mark).map_err(|e| e.to_string())?;
        if !lines.is_empty() {
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            emit_count += log_verify::parse_lines(&refs).emits.len();
            window.extend(lines);
        }
        if emit_count >= expected_emits.len() {
            break;
        }
        if daemon.poll_exit() {
            return Err(format!(
                "the daemon exited after {} of the expected {} emit lines",
                emit_count,
                expected_emits.len()
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out after 15 s waiting for {} emit lines; got {}",
                expected_emits.len(),
                emit_count
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }

    // Quiescence: keep reading until 500 ms pass with no new lines.
    let quiesce_deadline = Instant::now() + Duration::from_secs(15);
    let mut last_growth = Instant::now();
    loop {
        thread::sleep(Duration::from_millis(100));
        let lines = source.read_new(mark).map_err(|e| e.to_string())?;
        if !lines.is_empty() {
            window.extend(lines);
            last_growth = Instant::now();
        } else if Instant::now() - last_growth >= Duration::from_millis(500) {
            break;
        }
        if daemon.poll_exit() {
            return Err("the daemon exited while the log stream was still \
                        active"
                .to_string());
        }
        if Instant::now() >= quiesce_deadline {
            return Err("timed out waiting for the log stream to go \
                        quiescent"
                .to_string());
        }
    }

    Ok(window)
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

/// The directory containing the default config file (the daemon's CWD).
fn config_dir() -> PathBuf {
    keymapper::common::config_path::default_config_path()
        .expect("no default config path")
        .parent()
        .unwrap()
        .to_path_buf()
}

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

    // 3. Start the daemon and wait for its readiness line in the log stream.
    let mut daemon = DaemonChild::spawn(&config_dir());
    let mut source = daemon.log_source();
    wait_for_readiness(&mut source, &mut daemon);

    // 4. For each phase: hot-reload the config first (later phases) and wait
    //    for the daemon's hot-swap line, raise the log level to debug, inject
    //    the sequence once, collect and verify the log window, and reset the
    //    level.
    for (i, phase) in phases.iter().enumerate() {
        let content = phase_content(*phase);
        let sequences = build_test_sequences(&content);

        if i > 0 {
            eprintln!(
                "hot-reloading config (phase {} of {})...",
                i + 1,
                phases.len()
            );
            // Mark before the overwrite so the hot-swap line (logged at INFO,
            // before debug is raised) stays out of the phase's window.
            let reload_mark =
                source.mark().expect("failed to mark the log stream");
            config.overwrite(&content);
            wait_for_line(
                &mut source,
                &mut daemon,
                reload_mark,
                HOT_SWAP_LINE,
                Duration::from_secs(30),
            )
            .unwrap_or_else(|e| panic!("the config hot-reload failed: {e}"));
        }

        set_debug().expect("failed to raise the daemon's log level to debug");
        let mark = source.mark().expect("failed to mark the log stream");

        eprintln!(
            "phase {}: injecting {} steps, expecting {} emit lines...",
            i + 1,
            sequences.steps.len(),
            sequences.model.emits.len()
        );
        for step in &sequences.steps {
            inject_step(&*injector, step);
        }

        let window = collect_window(
            &mut source,
            &mut daemon,
            mark,
            &sequences.model.emits,
        )
        .unwrap_or_else(|e| panic!("phase {} failed: {e}", i + 1));
        reset_to_default()
            .expect("failed to reset the daemon's log level to info");

        let lines: Vec<&str> = window.iter().map(String::as_str).collect();
        log_verify::verify_window(&sequences.model, &lines).unwrap_or_else(
            |e| panic!("phase {} verification failed:\n{e}", i + 1),
        );
    }

    // 5. Teardown: stop the daemon; the config is restored via its Drop impl.
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
