// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux sandbox using uinput for virtual keyboards and evdev for monitoring.
//!
//! Two uinput devices are involved:
//!
//! 1. **Input device** (created by the sandbox): a virtual keyboard whose
//!    `/dev/input/event*` node the daemon opens and grabs. The sandbox injects
//!    events by writing to this device.
//! 2. **Output device** (created by the daemon): the daemon's own uinput
//!    device named `CrossPlatform_Virtual_Keyboard`. The sandbox discovers it
//!    by name and reads emitted events via `evdev::Device::fetch_events()`.

use std::{
    collections::HashSet,
    fs::{self, File},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},

    },
    path::Path,
    process,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use evdev::{Device, EventType};

use super::{CapturedEvent, Sandbox, SandboxError};

/// Unique device name prefix for sandbox input keyboards.
const INPUT_DEVICE_NAME_PREFIX: &str = "sandbox-input-";

/// Name pattern the daemon uses for its output uinput device.
const DAEMON_OUTPUT_DEVICE_NAME: &str = "CrossPlatform_Virtual_Keyboard";

// ---------------------------------------------------------------------------
// Shared event queue between the monitor thread and the test thread
// ---------------------------------------------------------------------------

/// Thread-safe event queue shared between the monitor tap and the sandbox.
struct EventQueue {
    events: Mutex<Vec<CapturedEvent>>,
}

impl EventQueue {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    fn push(&self, code: u16, is_down: bool) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(CapturedEvent { code, is_down });
    }

    fn drain(&self) -> Vec<CapturedEvent> {
        std::mem::take(
            &mut self.events.lock().unwrap_or_else(|e| e.into_inner()),
        )
    }
}

// ---------------------------------------------------------------------------
// Monitor thread handle
// ---------------------------------------------------------------------------

/// Handle for the background monitor thread that polls the daemon's output
/// device and records events into the shared queue.
struct MonitorHandle {
    /// Shutdown flag used to stop the monitor thread.
    shutdown_flag: Arc<AtomicBool>,

    /// Handle to the background thread.  Joined during teardown.
    thread_handle: Option<thread::JoinHandle<()>>,
}

// ---------------------------------------------------------------------------
// Sandbox implementation
// ---------------------------------------------------------------------------

/// Linux sandbox for end-to-end keyboard mapping tests.
pub struct LinuxSandbox {
    /// Virtual input device for event injection.  Only `Some` after a
    /// successful `setup()` call.
    device: Option<Arc<Mutex<uinput::Device>>>,

    /// Path to the input device's `/dev/input/event*` node, returned by
    /// `input_device_id()` so the daemon can target it.
    input_device_path: Option<String>,

    /// Shared event queue between the monitor thread and the test thread.
    queue: Arc<EventQueue>,

    /// Handle to the monitor thread and its shutdown mechanism.
    monitor: Option<MonitorHandle>,

    /// Flag indicating whether `setup()` has been called successfully.
    is_setup: bool,
}

impl LinuxSandbox {
    /// Check that `/dev/uinput` is accessible and writable.
    fn check_uinput() -> Result<(), SandboxError> {
        let path = Path::new("/dev/uinput");

        if !path.exists() {
            return Err(SandboxError::PermissionDenied(
                "/dev/uinput does not exist. Is the uinput kernel module \
                 loaded?"
                    .to_string(),
            ));
        }

        let metadata = fs::metadata(path).map_err(|e| {
            SandboxError::PermissionDenied(format!(
                "cannot stat /dev/uinput: {e}"
            ))
        })?;

        // Check if the current process can write to /dev/uinput by comparing
        // against the calling process's uid/gid and the file's permission
        // bits.
        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };

        let dev_uid = metadata.uid();
        let dev_gid = metadata.gid();
        let mode = metadata.mode();

        // Bitmasks from stat(2): owner, group, other permission bits.
        let r_w_x = 0o7;
        let owner_ok = dev_uid == uid && (mode & (r_w_x << 6)) != 0;
        let group_ok = dev_gid == gid && (mode & (r_w_x << 3)) != 0;
        let other_ok = (mode & r_w_x) != 0;

        // We only need write access, but checking execute as well catches the
        // common "no access at all" case.  A more precise check would test
        // only the write bit (0o2), but that misses the fact that character
        // devices also require some form of access permission.
        if !owner_ok && !group_ok && !other_ok {
            return Err(SandboxError::PermissionDenied(
                "cannot write to /dev/uinput. Add your user to the 'input' \
                 group or run with elevated privileges."
                    .to_string(),
            ));
        }

        Ok(())
    }

    /// Inject a keyboard event into the virtual input device.
    fn inject_key(&self, code: u16, value: i32) -> Result<(), SandboxError> {
        let device = self.device.as_ref().ok_or_else(|| {
            SandboxError::InjectionFailed(
                "sandbox not set up; call setup() first".to_string(),
            )
        })?;

        let mut dev = device
            .lock()
            .map_err(|e| SandboxError::InjectionFailed(format!("{e}")))?;

        dev.write(EventType::KEY.0 as _, code as i32, value)
            .map_err(|e| SandboxError::InjectionFailed(format!("{e}")))?;

        dev.synchronize()
            .map_err(|e| SandboxError::InjectionFailed(format!("{e}")))?;

        // Allow the kernel to propagate the event to readers of the event
        // node.  Without this delay the daemon may not have picked up the
        // event by the time the test proceeds.
        thread::sleep(Duration::from_millis(5));

        Ok(())
    }

    /// Start the background monitor thread if it is not already running.
    fn ensure_monitor(&mut self) {
        if self.monitor.is_some() {
            return;
        }

        let queue = Arc::clone(&self.queue);
        let shutdown = Arc::new(AtomicBool::new(false));

        let thread_handle = thread::Builder::new()
            .name("sandbox-monitor".to_string())
            .spawn({
                let shutdown = Arc::clone(&shutdown);
                move || {
                    // Wait briefly for the daemon to create its output device,
                    // then start polling.  The monitor thread re-tries
                    // discovery in case the daemon hasn't
                    // started yet.
                    let mut device: Option<Device> = None;

                    loop {
                        if shutdown.load(Ordering::Acquire) {
                            return;
                        }

                        // Try to discover the daemon's output device.
                        if device.is_none() {
                            if let Some(mon) =
                                find_device_by_name(DAEMON_OUTPUT_DEVICE_NAME)
                            {
                                device = Some(mon);
                            } else {
                                thread::sleep(Duration::from_millis(50));
                                continue;
                            }
                        }

                        if let Some(ref mut dev) = device {
                            // Fetch all pending events from the evdev device.
                            match dev.fetch_events() {
                                Ok(events) => {
                                    for event in events {
                                        if event.event_type() == EventType::KEY
                                        {
                                            let code = event.code();
                                            let value = event.value();

                                            // Skip the dummy KEY_0 (code 0)
                                            // events that sometimes appear
                                            // from
                                            // synchronization.
                                            if code == 0 {
                                                continue;
                                            }

                                            let is_down =
                                                value == 1 || value == 2;

                                            queue.push(code, is_down);
                                        }
                                    }
                                }
                                Err(e)
                                    if e.kind()
                                        == std::io::ErrorKind::WouldBlock =>
                                {
                                    // No events available; sleep and retry.
                                }
                                Err(e) => {
                                    eprintln!(
                                        "sandbox monitor: read error: {e}"
                                    );
                                }
                            }
                        }

                        thread::sleep(Duration::from_millis(10));
                    }
                }
            })
            .expect("failed to spawn sandbox monitor thread");

        self.monitor = Some(MonitorHandle {
            shutdown_flag: shutdown,
            thread_handle: Some(thread_handle),
        });
    }
}

impl Sandbox for LinuxSandbox {
    fn new() -> Result<Option<Self>, SandboxError> {
        Self::check_uinput()?;

        Ok(Some(Self {
            device: None,
            input_device_path: None,
            queue: Arc::new(EventQueue::new()),
            monitor: None,
            is_setup: false,
        }))
    }

    fn setup(&mut self) -> Result<(), SandboxError> {
        let (device, path) = create_uinput_device()?;

        self.device = Some(Arc::new(Mutex::new(device)));
        self.input_device_path = Some(path);
        self.is_setup = true;

        Ok(())
    }

    fn inject_key_down(&self, code: u16) -> Result<(), SandboxError> {
        // Value 1 = press, value 2 = auto-repeat (treated as press).
        self.inject_key(code, 1)
    }

    fn inject_key_up(&self, code: u16) -> Result<(), SandboxError> {
        // Value 0 = release.
        self.inject_key(code, 0)
    }

    fn drain_output_events(&self) -> Vec<CapturedEvent> {
        // Brief pause to allow pending events to propagate through the kernel
        // event queue before draining.
        thread::sleep(Duration::from_millis(50));
        self.queue.drain()
    }

    fn input_device_id(&self) -> Option<&str> {
        self.input_device_path.as_deref()
    }

    fn teardown(&mut self) {
        if !self.is_setup {
            return;
        }

        // Stop the monitor thread.
        if let Some(mut monitor) = self.monitor.take() {
            monitor.shutdown_flag.store(true, Ordering::Release);

            if let Some(jh) = monitor.thread_handle.take() {
                let _ = jh.join();
            }
        }

        // Drop the uinput device — the kernel automatically removes the
        // associated /dev/input/event* node when the file descriptor is
        // closed.
        self.device.take();
        self.input_device_path.take();

        self.is_setup = false;
    }
}

impl Drop for LinuxSandbox {
    fn drop(&mut self) {
        self.teardown();
    }
}

// ---------------------------------------------------------------------------
// Helper: scan /dev/input/event* nodes
// ---------------------------------------------------------------------------

/// Scan all `/dev/input/event*` nodes and return their `(rdev, path)` pairs.
fn scan_event_devices() -> Vec<(u64, String)> {
    let mut devices = Vec::new();

    let Ok(entries) = fs::read_dir("/dev/input") else {
        return devices;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.to_string_lossy().starts_with("/dev/input/event") {
            if let Ok(metadata) = fs::metadata(&path) {
                devices.push((
                    metadata.rdev(),
                    path.to_string_lossy().to_string(),
                ));
            }
        }
    }

    devices
}

// ---------------------------------------------------------------------------
// Helper: open device with non-blocking flag
// ---------------------------------------------------------------------------

/// Open an evdev device file in non-blocking mode.
///
/// `Device::open` always opens with `O_RDONLY`, but the monitor thread needs
/// `O_NONBLOCK` so that `fetch_events()` returns `WouldBlock` instead of
/// blocking when no events are available.
fn open_device_nonblock(path: &Path) -> std::io::Result<Device> {
    // Open the raw file descriptor with O_NONBLOCK.
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    let owned_fd: std::os::unix::io::OwnedFd = file.into();

    Device::from_fd(owned_fd)
}

// ---------------------------------------------------------------------------
// Helper: find device by name
// ---------------------------------------------------------------------------

/// Find a `/dev/input/event*` node whose device name matches `name`. Open
/// in non-blocking mode so the monitor thread can poll without blocking.
fn find_device_by_name(name: &str) -> Option<Device> {
    let Ok(entries) = fs::read_dir("/dev/input") else {
        return None;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.to_string_lossy().starts_with("/dev/input/event") {
            if let Ok(device) = open_device_nonblock(&path) {
                if device.name() == Some(name) {
                    return Some(device);
                }
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Helper: create the virtual input device
// ---------------------------------------------------------------------------

/// Create a uinput-based virtual keyboard and return the device along with
/// its `/dev/input/event*` node path.
///
/// The device name is `sandbox-input-{pid}` to avoid collisions with other
/// uinput devices and to make identification straightforward.
fn create_uinput_device() -> Result<(uinput::Device, String), SandboxError> {
    let device_name = format!("{INPUT_DEVICE_NAME_PREFIX}{}", process::id());

    // Snapshot existing event nodes so we can identify the new one after
    // creation.
    let before = scan_event_devices();

    let device = uinput::default()
        .map_err(|e| SandboxError::DeviceCreationFailed(format!("{e}")))?
        .name(&device_name)
        .map_err(|e| SandboxError::DeviceCreationFailed(format!("{e}")))?
        .event(uinput::event::Keyboard::All)
        .map_err(|e| SandboxError::DeviceCreationFailed(format!("{e}")))?
        .create()
        .map_err(|e| SandboxError::DeviceCreationFailed(format!("{e}")))?;

    // Small delay to let the kernel create the event node.
    thread::sleep(Duration::from_millis(50));

    // Find the newly created node by comparing device numbers.
    let after = scan_event_devices();
    let before_rdevs: HashSet<u64> =
        before.into_iter().map(|(r, _)| r).collect();

    after
        .into_iter()
        .find(|(r, _)| !before_rdevs.contains(r))
        .map(|(_, path)| (device, path))
        .ok_or_else(|| {
            SandboxError::DeviceCreationFailed(
                "uinput device was created but no new /dev/input/event* node \
                 appeared"
                    .to_string(),
            )
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_new_checks_uinput_access() {
        // If /dev/uinput is accessible, new() returns Some.  If not, it
        // returns a PermissionDenied error.  In both cases the behaviour is
        // well-defined.
        let result = LinuxSandbox::new();
        match result {
            Ok(Some(_)) => { /* sandbox is available */ }
            Ok(None) => panic!("new() returned None, expected Some or Err"),
            Err(SandboxError::PermissionDenied(msg)) => {
                eprintln!("skipping: sandbox unavailable ({msg})");
            }
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }

    #[test]
    fn sandbox_setup_requires_uinput_permission() {
        // This test exercises the full setup/teardown cycle.  It is skipped
        // when /dev/uinput is not accessible (e.g. in CI without privileges).
        let mut sandbox = match LinuxSandbox::new() {
            Ok(Some(s)) => s,
            Ok(None) => return, // platform not supported
            Err(e) => {
                eprintln!("skipping: {e}");
                return;
            }
        };

        if let Err(e) = sandbox.setup() {
            eprintln!("skipping: {e}");
            return;
        }

        assert!(
            sandbox.input_device_id().is_some(),
            "setup should set an input device path"
        );

        let path = sandbox.input_device_id().unwrap();
        assert!(
            path.starts_with("/dev/input/event"),
            "device path should be an event node: {path}"
        );

        sandbox.teardown();
        assert!(
            sandbox.input_device_id().is_none(),
            "teardown should clear the device path"
        );
    }

    #[test]
    fn inject_roundtrip() {
        // Full round-trip: setup, inject events, verify they appear on the
        // input device.  Since we don't have a running daemon in this unit
        // test, the injected events will not be remapped; they simply prove
        // that injection works.
        let mut sandbox = match LinuxSandbox::new() {
            Ok(Some(s)) => s,
            Ok(None) => return,
            Err(e) => {
                eprintln!("skipping: {e}");
                return;
            }
        };

        if let Err(e) = sandbox.setup() {
            eprintln!("skipping: {e}");
            return;
        }

        // Inject a key press and release.
        let result = sandbox.inject_key_down(28); // KEY_ENTER
        assert!(result.is_ok(), "inject_key_down should succeed: {result:?}");

        let result = sandbox.inject_key_up(28);
        assert!(result.is_ok(), "inject_key_up should succeed: {result:?}");

        sandbox.teardown();
    }
}
