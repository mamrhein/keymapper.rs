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
use std::{env, path::PathBuf, process::Command};

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
// Platform-specific key codes
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod codes {
    // CGKeyCode values (see src/platform/macos/key.rs)
    pub const CAPSLOCK: u16 = 57;
    pub const LEFT_CONTROL: u16 = 59;
    pub const A: u16 = 0;
    pub const B: u16 = 11;
}

#[cfg(target_os = "linux")]
mod codes {
    // Linux evdev key codes (see include/uapi/linux/input-event-codes.h)
    pub const CAPSLOCK: u16 = 58; // KEY_CAPSLOCK
    pub const LEFT_CONTROL: u16 = 29; // KEY_LEFTCTRL
    pub const A: u16 = 30; // KEY_A
    pub const B: u16 = 31; // KEY_B
}

#[cfg(target_os = "windows")]
mod codes {
    // Windows virtual-key codes (see WinUser.h)
    pub const CAPSLOCK: u16 = 0x14; // VK_CAPITAL
    pub const LEFT_CONTROL: u16 = 0xA2; // VK_LCONTROL
    pub const A: u16 = 0x41; // VK_A
    pub const B: u16 = 0x42; // VK_B
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

/// RAII guard that kills the daemon subprocess on `Drop`. Ensures cleanup
/// even when tests panic.
struct DaemonGuard {
    child: std::process::Child,
}

impl DaemonGuard {
    fn kill(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
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
fn start_daemon_in_dir(config_dir: &PathBuf) -> DaemonGuard {
    let child = Command::new(daemon_bin_path())
        .current_dir(config_dir)
        .spawn()
        .expect("failed to spawn keymapperd");
    DaemonGuard { child }
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
    let mut daemon = start_daemon_in_dir(&config_dir);

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
