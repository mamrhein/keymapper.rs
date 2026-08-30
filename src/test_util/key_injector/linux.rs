// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Linux keyboard injector using uinput for virtual keyboard creation.
//!
//! A single uinput device is created during `setup()`. The injector writes
//! events to this device and the daemon opens and grabs it for capture.

use std::{
    collections::HashSet,
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    process,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use evdev::{
    AttributeSet, InputEvent, KeyCode, MiscCode, uinput::VirtualDevice,
};

use super::{InjectorError, KeyInjector};
use crate::{HidUsage, platform::hid_translate::hid_usage_to_keycode};

/// Unique device name prefix for injector input keyboards.
pub const INPUT_DEVICE_NAME_PREFIX: &str = "virtual-keyboard";

/// Whether the platform injector can inject the given usage.
///
/// The forward table covers all of `HidUsage::ALL`, so every usage is
/// injectable.
pub fn is_injectable(usage: HidUsage) -> bool {
    hid_usage_to_keycode(usage).is_some()
}

// ---------------------------------------------------------------------------
// Injector implementation
// ---------------------------------------------------------------------------

/// Linux keyboard injector for end-to-end tests.
pub struct LinuxInjector {
    /// Virtual input device for event injection.  Only `Some` after a
    /// successful `setup()` call.
    device: Option<Arc<Mutex<VirtualDevice>>>,

    /// Path to the input device's `/dev/input/event*` node.  Exposed so
    /// the daemon can target it.
    input_device_path: Option<String>,

    /// Flag indicating whether `setup()` has been called successfully.
    is_setup: bool,
}

#[allow(dead_code)]
impl LinuxInjector {
    /// Check that `/dev/uinput` is accessible and writable.
    fn check_uinput() -> Result<(), InjectorError> {
        let path = Path::new("/dev/uinput");

        if !path.exists() {
            return Err(InjectorError::PermissionDenied(
                "/dev/uinput does not exist. Is the uinput kernel module \
                 loaded?"
                    .to_string(),
            ));
        }

        let metadata = fs::metadata(path).map_err(|e| {
            InjectorError::PermissionDenied(format!(
                "annot stat /dev/uinput: {e}"
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

        // Check group membership against both the effective GID and all
        // supplementary groups.  `getgroups(0, NULL)` returns the number of
        // supplementary groups without filling a buffer.
        let group_ok = if mode & (r_w_x << 3) == 0 {
            // No group permission bits set, short-circuit.
            false
        } else if dev_gid == gid {
            // Effective GID matches.
            true
        } else {
            // Check supplementary groups.  See getgroups(2).
            let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
            if count <= 0 {
                false
            } else {
                let mut groups = vec![0; count as usize];
                let filled =
                    unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
                if filled < 0 {
                    false
                } else {
                    groups[..filled as usize].contains(&dev_gid)
                }
            }
        };
        let other_ok = (mode & r_w_x) != 0;

        // We only need write access, but checking execute as well catches the
        // common "no access at all" case.  A more precise check would test
        // only the write bit (0o2), but that misses the fact that character
        // devices also require some form of access permission.
        if !owner_ok && !group_ok && !other_ok {
            return Err(InjectorError::PermissionDenied(match dev_gid {
                0 => "Only root can access /dev/uinput. Change the group or \
                      run with eleviated privileges."
                    .to_string(),
                _ => "Cannot write to /dev/uinput. Add your user to the its \
                      group (usually 'input') or run with elevated \
                      privileges."
                    .to_string(),
            }));
        }

        Ok(())
    }

    /// Inject a keyboard event into the virtual input device.
    fn inject_key(
        &self,
        usage: HidUsage,
        value: i32,
    ) -> Result<(), InjectorError> {
        const EV_KEY: u16 = 1;
        const EV_MSC: u16 = 4;
        const MSC_SCAN: u16 = 4;
        const EV_SYN: u16 = 0;
        const SYN_REPORT: u16 = 0;

        // The forward table is the same one the daemon's output path writes
        // with, so the injector and the daemon can never drift.
        let code = hid_usage_to_keycode(usage).ok_or_else(|| {
            InjectorError::NotSupported(format!(
                "no evdev key code for {}",
                usage.as_str()
            ))
        })?;

        let device = self.device.as_ref().ok_or_else(|| {
            InjectorError::InjectionFailed(
                "injector not set up; call setup() first".to_string(),
            )
        })?;

        let mut dev = device
            .lock()
            .map_err(|e| InjectorError::InjectionFailed(format!("{e}")))?;

        // A real HID keyboard emits an MSC_SCAN carrying the raw HID usage
        // `(page << 16) | id` before each key press; the daemon prefers it
        // over the EV_KEY code.  Key releases carry no scan code.
        let mut events: Vec<InputEvent> = Vec::new();
        if value == 1 {
            events.push(InputEvent::new(
                EV_MSC,
                MSC_SCAN,
                usage.code() as i32,
            ));
        }
        events.push(InputEvent::new(EV_KEY, code, value));
        events.push(InputEvent::new(EV_SYN, SYN_REPORT, 0));

        dev.emit(&events)
            .map_err(|e| InjectorError::InjectionFailed(format!("{e}")))?;

        // Allow the kernel to propagate the event to readers of the event
        // node.  Without this delay the daemon may not have picked up the
        // event by the time the test proceeds.
        thread::sleep(Duration::from_millis(5));

        Ok(())
    }
}

impl KeyInjector for LinuxInjector {
    fn new() -> Result<Option<Self>, InjectorError> {
        Self::check_uinput()?;

        Ok(Some(Self {
            device: None,
            input_device_path: None,
            is_setup: false,
        }))
    }

    fn setup(&mut self) -> Result<(), InjectorError> {
        let device_name =
            format!("{INPUT_DEVICE_NAME_PREFIX}-{}", process::id());
        let (device, path) = create_uinput_device(&device_name)?;

        self.device = Some(Arc::new(Mutex::new(device)));
        self.input_device_path = Some(path);

        self.is_setup = true;

        Ok(())
    }

    fn inject_key_down(&self, usage: HidUsage) -> Result<(), InjectorError> {
        // Value 1 = press, value 2 = auto-repeat (treated as press).
        self.inject_key(usage, 1)
    }

    fn inject_key_up(&self, usage: HidUsage) -> Result<(), InjectorError> {
        // Value 0 = release.
        self.inject_key(usage, 0)
    }

    fn teardown(&mut self) {
        if !self.is_setup {
            return;
        }

        // Drop the uinput device -- the kernel automatically removes the
        // associated /dev/input/event* node when the file descriptor is
        // closed.
        self.device.take();
        self.input_device_path.take();

        self.is_setup = false;
    }

    fn input_device_path(&self) -> Option<&str> {
        self.input_device_path.as_deref()
    }
}

impl Drop for LinuxInjector {
    fn drop(&mut self) {
        self.teardown();
    }
}

// ---------------------------------------------------------------------------
// Helper: create the virtual input device
// ---------------------------------------------------------------------------

/// Create a uinput-based virtual keyboard and return the device along with
/// its `/dev/input/event*` node path.
fn create_uinput_device(
    name: &str,
) -> Result<(VirtualDevice, String), InjectorError> {
    // Snapshot existing event nodes so we can identify the new one after
    // creation.
    let before = scan_event_devices();

    // KEY_CNT is the total number of key codes defined by the kernel
    // (linux/input.h: #define KEY_CNT (KEY_MAX + 1), where KEY_MAX = 0x2fd).
    const KEY_CNT: u16 = 0x2fe;
    let all_keys: AttributeSet<KeyCode> =
        (0..KEY_CNT).map(KeyCode::new).collect();

    // Declare MSC_SCAN support so the kernel accepts injected scan codes.
    let mut msc_codes = AttributeSet::<MiscCode>::new();
    msc_codes.insert(MiscCode::MSC_SCAN);

    let device = VirtualDevice::builder()
        .map_err(|e| {
            InjectorError::DeviceCreationFailed(format!(
                "failed to create virtual uinput device: {e}"
            ))
        })?
        .name(name)
        .with_keys(&all_keys)
        .map_err(|e| {
            InjectorError::DeviceCreationFailed(format!(
                "failed to enable key events: {e}"
            ))
        })?
        .with_msc(&msc_codes)
        .map_err(|e| {
            InjectorError::DeviceCreationFailed(format!(
                "failed to enable misc events: {e}"
            ))
        })?
        .build()
        .map_err(|e| {
            InjectorError::DeviceCreationFailed(format!(
                "failed to finalize virtual device: {e}"
            ))
        })?;

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
            InjectorError::DeviceCreationFailed(
                "uinput device was created but no new /dev/input/event* node \
                 appeared"
                    .to_string(),
            )
        })
}

/// Scan all `/dev/input/event*` nodes and return their `(rdev, path)` pairs.
fn scan_event_devices() -> Vec<(u64, String)> {
    let mut devices = Vec::new();

    let Ok(entries) = fs::read_dir("/dev/input") else {
        return devices;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.to_string_lossy().starts_with("/dev/input/event")
            && let Ok(metadata) = fs::metadata(&path)
        {
            devices
                .push((metadata.rdev(), path.to_string_lossy().to_string()));
        }
    }

    devices.sort_by_key(|(_, path)| path.clone());
    devices
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injector_new_checks_uinput_access() {
        // If /dev/uinput is accessible, new() returns Some.  If not, it
        // returns a PermissionDenied error.  In both cases the behaviour is
        // well-defined.
        let result = LinuxInjector::new();
        match result {
            Ok(Some(_)) => { /* injector is available */ }
            Ok(None) => panic!("new() returned None, expected Some or Err"),
            Err(InjectorError::PermissionDenied(msg)) => {
                eprintln!("skipping: injector unavailable ({msg})");
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[test]
    fn injector_setup_and_teardown() {
        // This test exercises the full setup/teardown cycle.  It is skipped
        // when /dev/uinput is not accessible (e.g. in CI without privileges).
        let mut injector = match LinuxInjector::new() {
            Ok(Some(i)) => i,
            Ok(None) => return, // platform not supported
            Err(e) => {
                eprintln!("skipping: {e}");
                return;
            }
        };

        if let Err(e) = injector.setup() {
            eprintln!("skipping: {e}");
            return;
        }

        assert!(
            injector.input_device_path().is_some(),
            "setup should set an input device path"
        );

        let path = injector.input_device_path().unwrap();
        assert!(
            path.starts_with("/dev/input/event"),
            "device path should be under /dev/input/"
        );

        injector.teardown();
    }

    #[test]
    fn inject_roundtrip() {
        let mut injector = match LinuxInjector::new() {
            Ok(Some(i)) => i,
            _ => return,
        };

        if injector.setup().is_err() {
            return;
        }

        // Inject a key-down and key-up for the "A" key.
        injector
            .inject_key_down(HidUsage::A)
            .expect("inject key down");
        injector.inject_key_up(HidUsage::A).expect("inject key up");

        injector.teardown();
    }

    #[test]
    fn is_injectable_covers_all_usages() {
        for usage in HidUsage::ALL {
            assert!(
                is_injectable(usage),
                "expected {usage} to be injectable on linux"
            );
        }
    }
}
