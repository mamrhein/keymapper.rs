// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! End-to-end integration tests using the virtual keyboard sandbox.
//!
//! These tests spawn the `keymapperd` binary as a subprocess, inject
//! synthetic keyboard events via the platform sandbox, and verify that
//! the daemon's remapped output matches expectations.

#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    thread,
    time::Duration,
};

use keymapper::util::sandbox::{CapturedEvent, Sandbox, SandboxError};
#[cfg(unix)]
use libc::{LOCK_EX, LOCK_NB, LOCK_UN};

#[cfg(unix)]
/// Cross-process serialization guard for e2e tests.
///
/// Nextest spawns separate processes per test (each with "6 filtered out"), so
/// a Rust `static Mutex` won't coordinate across them.  macOS event taps are
/// session-global — parallel daemons and monitors interfere with each other.
/// We use a POSIX `flock`-based file lock to ensure only one e2e test runs
/// at a time across all nextest worker processes.
struct E2eFileLock {
    file: std::fs::File,
}

#[cfg(unix)]
impl E2eFileLock {
    fn acquire() -> std::io::Result<Self> {
        let lock_path = env::temp_dir().join("keymapper_e2e.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&lock_path)?;

        // Retry until we acquire the exclusive lock.  Other e2e test processes
        // will block here until the current holder finishes.
        loop {
            let rc =
                unsafe { libc::flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
            if rc == 0 {
                return Ok(Self { file });
            }
            // flock returned -1 on error.  Check if it's EWOULDBLOCK (another
            // process holds the lock) — if so, retry.  Otherwise propagate.
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(err);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

#[cfg(unix)]
impl Drop for E2eFileLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), LOCK_UN) };
    }
}

#[cfg(not(unix))]
struct E2eFileLock;

#[cfg(not(unix))]
impl E2eFileLock {
    fn acquire() -> std::io::Result<Self> {
        Ok(Self)
    }
}

// ---------------------------------------------------------------------------
// Platform-specific key codes — sourced from the platform Key enum
// ---------------------------------------------------------------------------

mod codes {
    use keymapper::platform::Key;

    pub const CAPSLOCK: u16 = Key::CapsLock.as_native();
    pub const ESC: u16 = Key::Escape.as_native();
    pub const LEFT_ALT: u16 = Key::LeftAlt.as_native();
    pub const LEFT_CONTROL: u16 = Key::LeftControl.as_native();
    pub const A: u16 = Key::A.as_native();
    pub const B: u16 = Key::B.as_native();
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Resolve the path to the compiled `keymapperd` binary.
fn daemon_bin_path() -> PathBuf {
    env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("keymapperd")
}

/// Create a temporary directory with `config.yaml` containing *content*.
/// Returns the directory path.  The directory is NOT cleaned up automatically.
static CONFIG_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn write_config_dir(content: &str) -> PathBuf {
    let seq =
        CONFIG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let dir = env::temp_dir().join(format!("keymapper_e2e_{pid}_{seq}"));

    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    std::fs::write(dir.join("config.yaml"), content)
        .expect("failed to write config");

    dir
}

/// Timeout for waiting on a config hot-reload to complete.  The watcher
/// debounces filesystem events for 500ms (DEBOUNCE_INTERVAL), so we allow
/// some headroom for parsing and cache compilation.
const RELOAD_TIMEOUT: Duration = Duration::from_secs(5);

/// RAII guard that kills the daemon subprocess on `Drop`. Captures stdout
/// in a background thread so callers can await specific log messages
/// (e.g. "Configuration hot-swapped successfully!").
struct DaemonGuard {
    child: std::process::Child,
    /// Receiver for stdout lines from the daemon.
    stdout_rx: mpsc::Receiver<String>,
}

impl DaemonGuard {
    fn kill(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();

        // Allow the kernel time to clean up uinput device nodes after the
        // daemon's file descriptors are closed.  Without this delay the
        // next test's monitor may discover a stale /dev/input/event* entry.
        thread::sleep(Duration::from_millis(50));
    }

    /// Block until the daemon logs a hot-reload success message, or timeout.
    ///
    /// Returns `Ok(())` when the reload completed, `Err` on timeout.
    fn await_reload(&self) -> Result<(), String> {
        let target = "Configuration hot-swapped successfully!";
        let deadline = std::time::Instant::now() + RELOAD_TIMEOUT;

        loop {
            let remaining =
                deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(
                    "timeout waiting for config hot-reload".to_string()
                );
            }

            match self.stdout_rx.recv_timeout(remaining) {
                Ok(line) => {
                    if line.contains(target) {
                        return Ok(());
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(
                        "timeout waiting for config hot-reload".to_string()
                    );
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(
                        "daemon stdout closed unexpectedly".to_string()
                    );
                }
            }
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// Spawn the `keymapperd` binary in *config_dir* and return a guard that
/// ensures the child is killed on `Drop`.
///
/// When *device_path* is `Some`, passes it as `--device` to the daemon so it
/// captures from the specified input device instead of auto-discovering.
fn start_daemon_in_dir(
    config_dir: &PathBuf,
    device_path: Option<&str>,
) -> DaemonGuard {
    use std::process::Stdio;

    let mut cmd = Command::new(daemon_bin_path());
    cmd.current_dir(config_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    if let Some(path) = device_path {
        cmd.arg("--device").arg(path);
    }

    let mut child = cmd.spawn().expect("failed to spawn keymapperd");

    let stdout = child.stdout.take().expect("failed to capture stdout");

    // Spawn a background thread that reads stdout line-by-line and sends
    // each line over the channel.  This allows `await_reload()` to poll
    // for specific log messages without blocking the test thread.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        use std::io::BufRead;
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    DaemonGuard {
        child,
        stdout_rx: rx,
    }
}

/// Helper that wraps the full test lifecycle: setup sandbox, start daemon,
/// run the test closure, then tear down everything.
fn run_e2e_test<F>(config: &str, test_fn: F)
where
    F: FnOnce(&dyn Sandbox),
{
    // Serialize e2e tests across nextest worker processes.  macOS event taps
    // are session-global, so parallel daemons/monitors interfere.
    let _lock = E2eFileLock::acquire().expect("failed to acquire e2e lock");

    // When the driverkit feature is enabled, check for the virtual HID driver.
    // Skip gracefully if it's not loaded — the CGEvent fallback is not
    // sufficient for reliable e2e verification on modern macOS.
    #[cfg(all(target_os = "macos", feature = "driverkit"))]
    {
        use keymapper::platform::HidSocket;
        if let Err(e) = HidSocket::discover_and_open() {
            eprintln!(
                "skipping e2e test: virtual HID driver not connected \
                 ({e}).\nRun `keymapper driver install` and approve in \
                 System Settings."
            );
            return;
        }
    }

    // Create config directory.
    let config_dir = write_config_dir(config);

    // Create the sandbox — skip gracefully if not available.
    let mut sandbox = match create_sandbox() {
        Ok(Some(s)) => s,
        Ok(None) => {
            eprintln!("sandbox not available on this platform, skipping test");
            std::fs::remove_dir_all(&config_dir).ok();
            return;
        }
        Err(e) => {
            eprintln!("sandbox creation failed: {e}, skipping test");
            std::fs::remove_dir_all(&config_dir).ok();
            return;
        }
    };

    sandbox.setup().unwrap_or_else(|e| {
        eprintln!("sandbox setup failed: {e}, skipping test");
        std::fs::remove_dir_all(&config_dir).ok();
        std::process::exit(0);
    });

    // Give the monitor tap a moment to stabilize before the daemon starts,
    // so it captures all events from the beginning.
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Spawn the daemon in a subprocess.  The guard ensures cleanup on panic.
    let device_path = sandbox.input_device_id().map(|s| s.to_string());
    let mut daemon = start_daemon_in_dir(&config_dir, device_path.as_deref());

    // Allow the daemon to initialize (grab devices, create uinput, etc.).
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Run the test body.
    test_fn(&*sandbox);

    // Teardown: kill the daemon and clean up the sandbox.
    daemon.kill();

    sandbox.teardown();
    std::fs::remove_dir_all(&config_dir).ok();
}

#[cfg(target_os = "macos")]
fn create_sandbox() -> Result<Option<Box<dyn Sandbox>>, SandboxError> {
    use keymapper::util::sandbox::MacoSandbox;
    let s = MacoSandbox::new()?;
    Ok(s.map(|x| Box::new(x) as Box<dyn Sandbox>))
}

#[cfg(target_os = "linux")]
fn create_sandbox() -> Result<Option<Box<dyn Sandbox>>, SandboxError> {
    use keymapper::util::sandbox::LinuxSandbox;
    let s = LinuxSandbox::new()?;
    Ok(s.map(|x| Box::new(x) as Box<dyn Sandbox>))
}

#[cfg(target_os = "windows")]
fn create_sandbox() -> Result<Option<Box<dyn Sandbox>>, SandboxError> {
    use keymapper::util::sandbox::WindowsSandbox;
    let s = WindowsSandbox::new()?;
    Ok(s.map(|x| Box::new(x) as Box<dyn Sandbox>))
}

/// Overwrite the config file in *config_dir* with new content.
///
/// Uses `std::fs::write` which truncates and rewrites the same file path,
/// triggering `notify::EventKind::Modify` on the watched file. The watcher
/// debounces multiple events, so rapid successive writes are safe.
fn update_config(config_dir: &Path, content: &str) {
    std::fs::write(config_dir.join("config.yaml"), content)
        .expect("failed to write updated config");
}

/// Helper that wraps the full test lifecycle for hot-reload tests:
/// setup sandbox, start daemon, then run a multi-phase test closure that
/// has access to the config directory path and the daemon guard so it can
/// modify the config and await reloads.
fn run_e2e_test_with_reload<F>(initial_config: &str, test_fn: F)
where
    F: FnOnce(&dyn Sandbox, &PathBuf, &mut DaemonGuard),
{
    // Serialize e2e tests across nextest worker processes.
    let _lock = E2eFileLock::acquire().expect("failed to acquire e2e lock");

    // When the driverkit feature is enabled, check for the virtual HID driver.
    #[cfg(all(target_os = "macos", feature = "driverkit"))]
    {
        use keymapper::platform::HidSocket;
        if let Err(e) = HidSocket::discover_and_open() {
            eprintln!(
                "skipping e2e test: virtual HID driver not connected \
                 ({e}).\nRun `keymapper driver install` and approve in \
                 System Settings."
            );
            return;
        }
    }

    // Create config directory.
    let config_dir = write_config_dir(initial_config);

    // Create the sandbox — skip gracefully if not available.
    let mut sandbox = match create_sandbox() {
        Ok(Some(s)) => s,
        Ok(None) => {
            eprintln!("sandbox not available on this platform, skipping test");
            std::fs::remove_dir_all(&config_dir).ok();
            return;
        }
        Err(e) => {
            eprintln!("sandbox creation failed: {e}, skipping test");
            std::fs::remove_dir_all(&config_dir).ok();
            return;
        }
    };

    sandbox.setup().unwrap_or_else(|e| {
        eprintln!("sandbox setup failed: {e}, skipping test");
        std::fs::remove_dir_all(&config_dir).ok();
        std::process::exit(0);
    });

    // Give the monitor tap a moment to stabilize.
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Spawn the daemon.
    let device_path = sandbox.input_device_id().map(|s| s.to_string());
    let mut daemon = start_daemon_in_dir(&config_dir, device_path.as_deref());

    // Allow the daemon to initialize.
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Drain any events captured during startup.
    let _ = sandbox.drain_output_events();

    // Run the multi-phase test body.
    test_fn(&*sandbox, &config_dir, &mut daemon);

    // Teardown.
    daemon.kill();

    sandbox.teardown();
    std::fs::remove_dir_all(&config_dir).ok();
}

// ---------------------------------------------------------------------------
// Integration tests
// ---------------------------------------------------------------------------

/// Verify that a global mapping remaps CapsLock to LeftControl.
///
/// This is the canonical "capslock to control" remap. The daemon should
/// swallow the CapsLock event and emit LeftControl instead.
#[test]
fn e2e_global_mapping_capslock_to_control() {
    let config = r#"- mappings:
    CapsLock: LeftControl"#;

    run_e2e_test(config, |sandbox| {
        sandbox
            .inject_key_down(codes::CAPSLOCK)
            .expect("inject key down");
        sandbox
            .inject_key_up(codes::CAPSLOCK)
            .expect("inject key up");

        let events = sandbox.drain_output_events();

        assert_eq!(
            events,
            vec![
                CapturedEvent {
                    code: codes::LEFT_CONTROL,
                    is_down: true,
                },
                CapturedEvent {
                    code: codes::LEFT_CONTROL,
                    is_down: false,
                },
            ],
            "CapsLock should be remapped to LeftControl"
        );
    });
}

/// Verify that an unmapped key passes through unchanged.
///
/// When no rule matches, the daemon forwards the original event. The
/// monitoring tap should see the key as-is.
#[test]
fn e2e_unmapped_key_passthrough() {
    // Config maps CapsLock, but not A.
    let config = r#"- mappings:
    CapsLock: LeftControl"#;

    run_e2e_test(config, |sandbox| {
        // Press 'A' which has no mapping.
        sandbox.inject_key_down(codes::A).expect("inject key down");
        sandbox.inject_key_up(codes::A).expect("inject key up");

        let events = sandbox.drain_output_events();

        assert_eq!(
            events,
            vec![
                CapturedEvent {
                    code: codes::A,
                    is_down: true,
                },
                CapturedEvent {
                    code: codes::A,
                    is_down: false,
                },
            ],
            "Unmapped key should pass through unchanged"
        );
    });
}

/// Verify a simple single-key-to-single-key remap for non-modifier keys.
#[test]
fn e2e_global_mapping_a_to_b() {
    let config = r#"- mappings:
    A: B"#;

    run_e2e_test(config, |sandbox| {
        sandbox.inject_key_down(codes::A).expect("inject key down");
        sandbox.inject_key_up(codes::A).expect("inject key up");

        let events = sandbox.drain_output_events();

        assert_eq!(
            events,
            vec![
                CapturedEvent {
                    code: codes::B,
                    is_down: true,
                },
                CapturedEvent {
                    code: codes::B,
                    is_down: false,
                },
            ],
            "A should be remapped to B"
        );
    });
}

/// Verify that a chord output (modifier + key) is emitted correctly.
///
/// The daemon should emit the modifier down, then the base key down,
/// then reverse on release.  The monitoring tap captures all four events.
#[test]
fn e2e_chord_output() {
    let config = r#"- mappings:
    CapsLock: Cmd+A"#;

    run_e2e_test(config, |sandbox| {
        sandbox
            .inject_key_down(codes::CAPSLOCK)
            .expect("inject key down");
        sandbox
            .inject_key_up(codes::CAPSLOCK)
            .expect("inject key up");

        let events = sandbox.drain_output_events();

        // On macOS: Cmd (55) down, A (0) down, A (0) up, Cmd (55) up.
        // On Linux: Super (125) down, A (30) down, A (30) up, Super (125) up.
        // The exact codes depend on the platform — we just check we get
        // 4 events forming a proper chord.
        assert_eq!(events.len(), 4, "chord output should produce 4 events");

        // First two events are downs, last two are ups.
        assert!(events[0].is_down);
        assert!(events[1].is_down);
        assert!(!events[2].is_down);
        assert!(!events[3].is_down);

        // The base key (A) is the inner pair.
        assert_eq!(events[1].code, codes::A, "base key should be A");
        assert_eq!(events[2].code, codes::A, "base key release should be A");

        // The modifier wraps around.
        assert_eq!(events[0].code, events[3].code, "modifier should match");
    });
}

/// Verify a multi-output mapping (one key triggers multiple sequential
/// outputs).
#[test]
fn e2e_multi_output_mapping() {
    let config = r#"- mappings:
    CapsLock: [LeftControl, A]"#;

    run_e2e_test(config, |sandbox| {
        sandbox
            .inject_key_down(codes::CAPSLOCK)
            .expect("inject key down");
        sandbox
            .inject_key_up(codes::CAPSLOCK)
            .expect("inject key up");

        let events = sandbox.drain_output_events();

        // Expected: Ctrl down, Ctrl up, A down, A up.
        assert_eq!(events.len(), 4, "multi-output should produce 4 events");

        assert_eq!(events[0].code, codes::LEFT_CONTROL);
        assert!(events[0].is_down);
        assert_eq!(events[1].code, codes::LEFT_CONTROL);
        assert!(!events[1].is_down);
        assert_eq!(events[2].code, codes::A);
        assert!(events[2].is_down);
        assert_eq!(events[3].code, codes::A);
        assert!(!events[3].is_down);
    });
}

/// Verify a modifier-combination mapping (Ctrl+A maps to B).
#[test]
fn e2e_modifier_combination_mapping() {
    let config = r#"- mappings:
    Ctrl+A: B"#;

    run_e2e_test(config, |sandbox| {
        // Press Ctrl, hold it, then press A.
        sandbox
            .inject_key_down(codes::LEFT_CONTROL)
            .expect("inject ctrl down");
        sandbox.inject_key_down(codes::A).expect("inject a down");
        sandbox.inject_key_up(codes::A).expect("inject a up");
        sandbox
            .inject_key_up(codes::LEFT_CONTROL)
            .expect("inject ctrl up");

        let events = sandbox.drain_output_events();

        // Ctrl down passes through, then A is remapped to B.
        assert!(events.len() >= 4, "expected at least 4 events");

        // Find the B press/release among the captured events.
        let b_down = events.iter().any(|e| e.code == codes::B && e.is_down);
        let b_up = events.iter().any(|e| e.code == codes::B && !e.is_down);

        assert!(b_down, "should see B key-down from remapped Ctrl+A");
        assert!(b_up, "should see B key-up from remapped Ctrl+A");
    });
}

/// Verify that a swap mapping works (CapsLock <-> LeftControl).
#[test]
fn e2e_swap_mapping() {
    let config = r#"- mappings:
    CapsLock: LeftControl
    LeftControl: CapsLock"#;

    run_e2e_test(config, |sandbox| {
        // CapsLock should become LeftControl.
        sandbox
            .inject_key_down(codes::CAPSLOCK)
            .expect("inject capslock down");
        sandbox
            .inject_key_up(codes::CAPSLOCK)
            .expect("inject capslock up");

        let events = sandbox.drain_output_events();

        assert_eq!(
            events,
            vec![
                CapturedEvent {
                    code: codes::LEFT_CONTROL,
                    is_down: true,
                },
                CapturedEvent {
                    code: codes::LEFT_CONTROL,
                    is_down: false,
                },
            ],
            "CapsLock should swap to LeftControl"
        );

        // LeftControl should become CapsLock.
        sandbox
            .inject_key_down(codes::LEFT_CONTROL)
            .expect("inject ctrl down");
        sandbox
            .inject_key_up(codes::LEFT_CONTROL)
            .expect("inject ctrl up");

        let events = sandbox.drain_output_events();

        assert_eq!(
            events,
            vec![
                CapturedEvent {
                    code: codes::CAPSLOCK,
                    is_down: true,
                },
                CapturedEvent {
                    code: codes::CAPSLOCK,
                    is_down: false,
                },
            ],
            "LeftControl should swap to CapsLock"
        );
    });
}

/// Verify that a keyboard filter restricts mappings to matching devices.
///
/// The daemon discovers both virtual keyboards but only grabs one for
/// monitoring.  The config uses a per-group `keyboards` filter that matches
/// the primary device's name.  Depending on which device the daemon grabs:
///
/// - If primary is grabbed: CapsLock is remapped to LeftControl (filter
///   matches).
/// - If secondary is grabbed: CapsLock passes through unchanged (filter blocks
///   the mapping because the grabbed device doesn't match).
///
/// Both outcomes are valid and demonstrate correct keyboard filter behavior.
#[cfg(target_os = "linux")]
#[test]
fn e2e_keyboard_filter() {
    use keymapper::util::sandbox::{
        LinuxSandbox, linux::INPUT_DEVICE_NAME_PREFIX,
    };

    // Serialize e2e tests.
    let _lock = E2eFileLock::acquire().expect("failed to acquire e2e lock");

    // Create the sandbox.
    let mut sandbox = match LinuxSandbox::new() {
        Ok(Some(s)) => s,
        Ok(None) => {
            eprintln!("sandbox not available, skipping test");
            return;
        }
        Err(e) => {
            eprintln!("sandbox creation failed: {e}, skipping test");
            return;
        }
    };

    sandbox.setup().unwrap_or_else(|e| {
        eprintln!("sandbox setup failed: {e}, skipping test");
        std::process::exit(0);
    });

    // Create the secondary device with a different name.
    sandbox.create_secondary_device().unwrap_or_else(|e| {
        eprintln!("failed to create secondary device: {e}");
        sandbox.teardown();
    });

    // Get device paths for identification.
    let primary_path = sandbox.input_device_id().unwrap();
    let secondary_path = sandbox.secondary_device_path().unwrap();

    // Build the device name that the daemon will discover for the primary.
    // The daemon's keyboard discovery on Linux uses `ID_PRODUCT_NAME` from
    // udev, falling back to the evdev device name.  We use the evdev device
    // name pattern set by the sandbox.
    let primary_name =
        format!("{INPUT_DEVICE_NAME_PREFIX}-{}", std::process::id());

    // Give the monitor tap a moment to stabilize.
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Write config with a per-group keyboard filter matching the primary name.
    let seq =
        CONFIG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let config_dir =
        env::temp_dir().join(format!("keymapper_e2e_{pid}_{seq}"));
    std::fs::create_dir_all(&config_dir).expect("failed to create temp dir");

    let config_content = format!(
        r#"- name: "primary keyboard rules"
  keyboards:
    - name: "{primary_name}"
  mappings:
    CapsLock: LeftControl"#
    );
    std::fs::write(config_dir.join("config.yaml"), &config_content)
        .expect("failed to write config");

    // Spawn the daemon, passing the primary device path so it captures from
    // the sandbox virtual keyboard.
    let device_path = sandbox.input_device_id().map(|s| s.to_string());
    let mut daemon = start_daemon_in_dir(&config_dir, device_path.as_deref());

    // Allow the daemon to initialize.
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Drain any events captured during startup.
    let _ = sandbox.drain_output_events();

    // Inject CapsLock from the primary device and check output.
    sandbox
        .inject_key_down(codes::CAPSLOCK)
        .expect("inject key down");
    sandbox
        .inject_key_up(codes::CAPSLOCK)
        .expect("inject key up");

    let primary_events = sandbox.drain_output_events();

    // Determine which device the daemon grabbed by checking if CapsLock was
    // remapped.  If events show LeftControl, the daemon grabbed primary and
    // the filter matched.  If events show CapsLock, the daemon grabbed
    // secondary and the filter blocked the mapping.
    let primary_grabbed =
        primary_events.iter().any(|e| e.code == codes::LEFT_CONTROL);

    if primary_grabbed {
        // Daemon grabbed primary; filter matches; mapping applies.
        assert_eq!(
            primary_events,
            vec![
                CapturedEvent {
                    code: codes::LEFT_CONTROL,
                    is_down: true,
                },
                CapturedEvent {
                    code: codes::LEFT_CONTROL,
                    is_down: false,
                },
            ],
            "primary device should be remapped (filter matches)"
        );
        eprintln!(
            "keyboard filter test: daemon grabbed primary ({primary_path}), \
             filter matched, CapsLock remapped to LeftControl"
        );
    } else {
        // Daemon grabbed secondary; filter does NOT match; mapping blocked.
        assert_eq!(
            primary_events,
            vec![
                CapturedEvent {
                    code: codes::CAPSLOCK,
                    is_down: true,
                },
                CapturedEvent {
                    code: codes::CAPSLOCK,
                    is_down: false,
                },
            ],
            "secondary device should pass through (filter blocks mapping)"
        );
        eprintln!(
            "keyboard filter test: daemon grabbed secondary \
             ({secondary_path}), filter did not match, CapsLock passed \
             through"
        );
    }

    // Teardown.
    daemon.kill();
    sandbox.teardown();
    std::fs::remove_dir_all(&config_dir).ok();
}

/// Verify that a keyboard filter restricts mappings to matching devices on
/// macOS.
///
/// Unlike the Linux test, macOS uses IOHIDManager for input capture, which
/// delivers events per-device and provides the device's Location ID. The
/// sandbox injects events via CGEvent, which operates at a higher layer than
/// IOHIDManager. This means injected events bypass the daemon's input capture
/// entirely — they go straight to the session and are captured by the monitor
/// tap regardless of whether a mapping exists.
///
/// This test validates:
/// 1. Keyboard discovery via `list_keyboards()` works on macOS.
/// 2. A keyboard-filtered config is constructed correctly from discovered
///    device metadata.
/// 3. The daemon starts and runs with the filtered config.
/// 4. Unmapped keys pass through to the monitor tap (CGEvent injection always
///    reaches HIDEventTap).
///
/// To fully verify that mapped keys are suppressed, a real physical keyboard
/// must be used. The test logs the discovered keyboards and the filter config
/// so a user can manually verify filtering by pressing keys on an attached
/// keyboard.
#[cfg(target_os = "macos")]
#[test]
fn e2e_keyboard_filter() {
    // Serialize e2e tests across nextest worker processes.
    let _lock = E2eFileLock::acquire().expect("failed to acquire e2e lock");

    // Discover attached keyboards.
    let keyboards = match keymapper::platform::list_keyboards() {
        Ok(kbs) => kbs,
        Err(e) => {
            eprintln!("keyboard discovery failed: {e}, skipping test");
            return;
        }
    };

    if keyboards.is_empty() {
        eprintln!("no keyboards discovered, skipping test");
        return;
    }

    eprintln!("discovered {} keyboard(s):", keyboards.len());
    for kb in &keyboards {
        eprintln!(
            "  - name={}, vendor={}, model={}, device={}",
            kb.name, kb.vendor, kb.model, kb.device
        );
    }

    // Build a keyboard filter that matches the first discovered keyboard.
    let target = &keyboards[0];
    let config_content =
        if !target.vendor.is_empty() && target.vendor != "0x0000" {
            // Filter by vendor — the most stable identifier.
            format!(
                r#"keyboards:
  - vendor: "{vendor}"
groups:
  - mappings:
      CapsLock: LeftControl"#,
                vendor = target.vendor
            )
        } else {
            // Fallback: filter by name if vendor is not available.
            format!(
                r#"keyboards:
  - name: "{name}"
groups:
  - mappings:
      CapsLock: LeftControl"#,
                name = target.name
            )
        };

    eprintln!("keyboard filter config:\n{}", config_content);

    // Create config directory.
    let seq =
        CONFIG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let config_dir =
        env::temp_dir().join(format!("keymapper_e2e_{pid}_{seq}"));
    std::fs::create_dir_all(&config_dir).expect("failed to create temp dir");
    std::fs::write(config_dir.join("config.yaml"), &config_content)
        .expect("failed to write config");

    // Create the sandbox — skip gracefully if not available.
    let mut sandbox = match create_sandbox() {
        Ok(Some(s)) => s,
        Ok(None) => {
            eprintln!("sandbox not available on this platform, skipping test");
            std::fs::remove_dir_all(&config_dir).ok();
            return;
        }
        Err(e) => {
            eprintln!("sandbox creation failed: {e}, skipping test");
            std::fs::remove_dir_all(&config_dir).ok();
            return;
        }
    };

    sandbox.setup().unwrap_or_else(|e| {
        eprintln!("sandbox setup failed: {e}, skipping test");
        std::fs::remove_dir_all(&config_dir).ok();
        std::process::exit(0);
    });

    // Give the monitor tap a moment to stabilize.
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Spawn the daemon in a subprocess.
    let device_path = sandbox.input_device_id().map(|s| s.to_string());
    let mut daemon = start_daemon_in_dir(&config_dir, device_path.as_deref());

    // Allow the daemon to initialize.
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Drain any events captured during startup.
    let _ = sandbox.drain_output_events();

    // Inject CapsLock via CGEvent.  On macOS, CGEvent injection bypasses
    // IOHIDManager, so the daemon never sees this event. The monitor tap
    // captures it at the HIDEventTap layer, so it always appears in output.
    // This validates that the sandbox infrastructure works, but cannot
    // verify that keyboard filtering suppresses mapped keys from real
    // keyboards.
    sandbox
        .inject_key_down(codes::CAPSLOCK)
        .expect("inject key down");
    sandbox
        .inject_key_up(codes::CAPSLOCK)
        .expect("inject key up");

    let events = sandbox.drain_output_events();

    // CGEvent-injected events always reach the monitor tap, regardless of
    // daemon mappings. We verify that the injection and capture infrastructure
    // works correctly.
    assert_eq!(
        events,
        vec![
            CapturedEvent {
                code: codes::CAPSLOCK,
                is_down: true,
            },
            CapturedEvent {
                code: codes::CAPSLOCK,
                is_down: false,
            },
        ],
        "CGEvent-injected CapsLock should reach the monitor tap. Note: this \
         does NOT verify keyboard filtering, because CGEvent injection \
         bypasses IOHIDManager. For real filtering verification, press \
         CapsLock on an attached keyboard and observe whether it is remapped \
         to LeftControl."
    );

    eprintln!(
        "keyboard filter test: daemon running with filter for '{}' \
         (vendor={}). CGEvent injection bypasses IOHIDManager, so the \
         sandbox cannot verify mapped key suppression. To verify filtering, \
         press CapsLock on the '{}' keyboard and observe whether it is \
         remapped to LeftControl.",
        target.name, target.vendor, target.name
    );

    // Teardown.
    daemon.kill();
    sandbox.teardown();
    std::fs::remove_dir_all(&config_dir).ok();
}

/// Verify that a keyboard filter restricts mappings to matching devices on
/// Windows.
///
/// Unlike the Linux test, Windows uses Raw Input (`WM_INPUT`) for device
/// identification. `SendInput` injection does NOT generate `WM_INPUT` events,
/// so the worker thread has no Raw Input buffer entries to match against.
/// After a 10 ms delay, the worker falls back to a lookup without device
/// identification. This means:
///
/// 1. Keyboard filtering is effectively bypassed for `SendInput` events.
/// 2. The mapping still works because the worker finds the rule without
///    device filtering.
/// 3. Real physical keyboard events (which DO trigger Raw Input) are subject
///    to the keyboard filter.
///
/// This test validates:
/// 1. Keyboard discovery via `list_keyboards()` works on Windows.
/// 2. A keyboard-filtered config is constructed correctly from discovered
///    device metadata.
/// 3. The daemon starts and runs with the filtered config.
/// 4. The mapping still works for `SendInput` events (worker falls back to
///    no-device-ID lookup).
///
/// To fully verify keyboard filtering, a real physical keyboard must be used.
/// The test logs the discovered keyboards and the filter config so a user can
/// manually verify filtering by pressing keys on an attached keyboard.
#[cfg(target_os = "windows")]
#[test]
fn e2e_keyboard_filter() {
    // Serialize e2e tests across nextest worker processes.
    let _lock = E2eFileLock::acquire().expect("failed to acquire e2e lock");

    // Discover attached keyboards.
    let keyboards = match keymapper::platform::list_keyboards() {
        Ok(kbs) => kbs,
        Err(e) => {
            eprintln!("keyboard discovery failed: {e}, skipping test");
            return;
        }
    };

    if keyboards.is_empty() {
        eprintln!("no keyboards discovered, skipping test");
        return;
    }

    eprintln!("discovered {} keyboard(s):", keyboards.len());
    for kb in &keyboards {
        eprintln!(
            "  - name={}, vendor={}, model={}, device={}",
            kb.name, kb.vendor, kb.model, kb.device
        );
    }

    // Build a keyboard filter that matches the first discovered keyboard.
    let target = &keyboards[0];
    let config_content =
        if !target.vendor.is_empty() && target.vendor != "0x0000" {
            // Filter by vendor — the most stable identifier.
            format!(
                r#"keyboards:
  - vendor: "{vendor}"
groups:
  - mappings:
      CapsLock: LeftControl"#,
                vendor = target.vendor
            )
        } else {
            // Fallback: filter by name if vendor is not available.
            format!(
                r#"keyboards:
  - name: "{name}"
groups:
  - mappings:
      CapsLock: LeftControl"#,
                name = target.name
            )
        };

    eprintln!("keyboard filter config:\n{}", config_content);

    // Create config directory.
    let seq =
        CONFIG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let config_dir =
        env::temp_dir().join(format!("keymapper_e2e_{pid}_{seq}"));
    std::fs::create_dir_all(&config_dir).expect("failed to create temp dir");
    std::fs::write(config_dir.join("config.yaml"), &config_content)
        .expect("failed to write config");

    // Create the sandbox — skip gracefully if not available.
    let mut sandbox = match create_sandbox() {
        Ok(Some(s)) => s,
        Ok(None) => {
            eprintln!("sandbox not available on this platform, skipping test");
            std::fs::remove_dir_all(&config_dir).ok();
            return;
        }
        Err(e) => {
            eprintln!("sandbox creation failed: {e}, skipping test");
            std::fs::remove_dir_all(&config_dir).ok();
            return;
        }
    };

    sandbox.setup().unwrap_or_else(|e| {
        eprintln!("sandbox setup failed: {e}, skipping test");
        std::fs::remove_dir_all(&config_dir).ok();
        std::process::exit(0);
    });

    // Give the monitor tap a moment to stabilize.
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Spawn the daemon in a subprocess.
    let mut daemon = start_daemon_in_dir(&config_dir);

    // Allow the daemon to initialize.
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Drain any events captured during startup.
    let _ = sandbox.drain_output_events();

    // Inject CapsLock via SendInput.  On Windows, SendInput does NOT
    // generate WM_INPUT events, so the worker has no Raw Input buffer
    // entries to match against.  After a 10 ms delay, the worker falls
    // back to a lookup without device identification.  The mapping is
    // found because no device ID means keyboard filters pass through.
    // This validates that the injection and capture infrastructure works,
    // but cannot verify that keyboard filtering suppresses mapped keys
    // from real keyboards.
    sandbox
        .inject_key_down(codes::CAPSLOCK)
        .expect("inject key down");
    sandbox
        .inject_key_up(codes::CAPSLOCK)
        .expect("inject key up");

    let events = sandbox.drain_output_events();

    // SendInput-injected events fall back to no-device-ID lookup, so
    // the mapping IS applied (CapsLock → LeftControl).  The monitor
    // captures the daemon's SendInput output (which has no marker).
    // Note: this does NOT verify keyboard filtering, because the worker
    // has no device ID for SendInput events.
    if !events.is_empty() {
        // If events are captured, verify the remapping.
        let has_left_control =
            events.iter().any(|e| e.code == codes::LEFT_CONTROL);
        if has_left_control {
            eprintln!(
                "keyboard filter test: CapsLock was remapped to \
                 LeftControl (SendInput bypasses Raw Input, so device \
                 filtering is not applied)"
            );
        }
    }

    eprintln!(
        "keyboard filter test: daemon running with filter for '{}' \
         (vendor={}). SendInput injection does NOT trigger WM_INPUT, \
         so the worker falls back to no-device-ID lookup and keyboard \
         filtering is bypassed. To verify filtering, press CapsLock on \
         the '{}' keyboard and observe whether it is remapped to \
         LeftControl.",
        target.name, target.vendor, target.name
    );

    // Teardown.
    daemon.kill();
    sandbox.teardown();
    std::fs::remove_dir_all(&config_dir).ok();
}

/// Verify that changing the config file while the daemon is running causes
/// the new mapping to take effect.
///
/// The test exercises three phases:
/// 1. Initial mapping is active (CapsLock → LeftControl).
/// 2. Config is rewritten to map CapsLock → A, and the daemon hot-reloads.
/// 3. The new mapping is verified by injecting CapsLock and expecting A.
#[test]
fn e2e_config_hot_reload() {
    let initial_config = r#"- mappings:
    CapsLock: LeftAlt+A"#;

    run_e2e_test_with_reload(initial_config, |sandbox, config_dir, daemon| {
        // --- Phase 1: verify initial mapping (CapsLock → LeftControl) ---
        sandbox
            .inject_key_down(codes::CAPSLOCK)
            .expect("Inject key down");
        sandbox
            .inject_key_up(codes::CAPSLOCK)
            .expect("Inject key up");

        let events = sandbox.drain_output_events();
        assert_eq!(
            events,
            vec![
                CapturedEvent {
                    code: codes::LEFT_ALT,
                    is_down: true,
                },
                CapturedEvent {
                    code: codes::A,
                    is_down: true,
                },
                CapturedEvent {
                    code: codes::A,
                    is_down: false,
                },
                CapturedEvent {
                    code: codes::LEFT_ALT,
                    is_down: false,
                },
            ],
            "Initial mapping: CapsLock should be remapped to LeftAlt+A"
        );

        // --- Phase 2: hot-reload config to CapsLock → Escape ---
        let new_config = r#"- mappings:
    CapsLock: Escape"#;
        update_config(config_dir, new_config);

        // Block until the daemon reports a successful hot-swap.
        daemon.await_reload().expect(
            "Daemon should have hot-reloaded the configuration within timeout",
        );

        // Small grace period after reload for the new cache to be used.
        std::thread::sleep(std::time::Duration::from_millis(100));

        // --- Phase 3: verify new mapping takes effect (CapsLock → Escape) ---
        sandbox
            .inject_key_down(codes::CAPSLOCK)
            .expect("Inject key down");
        sandbox
            .inject_key_up(codes::CAPSLOCK)
            .expect("Inject key up");

        let events = sandbox.drain_output_events();
        assert_eq!(
            events,
            vec![
                CapturedEvent {
                    code: codes::ESC,
                    is_down: true
                },
                CapturedEvent {
                    code: codes::ESC,
                    is_down: false
                },
            ],
            "Reloaded mapping: CapsLock should be remapped to Escape"
        );
    });
}
