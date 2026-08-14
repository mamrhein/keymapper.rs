# macOS — IOKit Input Capture and DriverKit Output

keymapper uses two kernel-adjacent mechanisms on macOS:

1. **IOKit device seizure** — captures input by opening the physical keyboard via `IOHIDManager`, filtering it out of the normal event stream, and reading raw HID reports directly.
2. **DriverKit virtual HID driver** — emits remapped key events through a virtual USB HID keyboard device, which the macOS I/O HID stack treats as input from a real physical keyboard.

Together, these give reliable, hardware-level interception and injection that works across all applications.

## How it works

### Input capture (IOKit device seizure)

The `keymapperd` daemon runs as root. At startup, it creates an `IOHIDMatching` dictionary for keyboard-class devices, calls `IOHIDManagerCopyDevices()`, and opens each matched device with `IOHIDDeviceOpen()`. Opening a device in this mode removes it from the standard event stream — macOS no longer delivers those keystrokes to applications or to `CGEventTap` listeners.

The daemon then creates an `IOHIDQueue` per device and registers a value-available callback. Every raw HID report arrives in this callback, where it is decoded into key events, matched against the remapping configuration, and forwarded to the output stage.

Because device seizure requires privileged access, `keymapperd` runs as a **LaunchDaemon** (`/Library/LaunchDaemons/de.adrhinum.keymapperd.plist`) rather than a user-level LaunchAgent.

### Output emission (DriverKit virtual HID driver)

The `KeyMapperVirtualHID` DriverKit extension exposes a virtual USB HID keyboard device. When a remapped key is produced, `keymapperd` sends the resulting USB HID keyboard report to the driver over a Unix domain socket. The macOS I/O HID stack treats this as input from a physical keyboard, so it works in all applications — including those that use raw input APIs or have accessibility restrictions.

Communication between `keymapperd` and the driver is handled through a Unix domain socket at `/tmp/keymapper-vhid.sock`.

### Why this approach?

Compared to `CGEventTap`-based interception:

| Aspect | CGEventTap | IOKit seizure + DriverKit |
|--------|-----------|--------------------------|
| Event capture | Tap into system event stream | Open the physical device directly |
| Global menus | Events visible | Seized events do not reach menus; handled explicitly |
| Accessibility permission | Required | Not required (runs as root daemon) |
| Reliability | Can be blocked by other taps or app restrictions | Hardware-level, works everywhere |
| Injection | Synthetic `CGEvent` (may be blocked) | Virtual HID device (hardware-level) |

## Installation

### Via Homebrew

```bash
brew install keymapper
```

The Homebrew formula builds both the Rust binaries and the DriverKit extension from source. Requires Xcode command-line tools.

Start the LaunchDaemon:

```bash
brew services start keymapper
```

### From source (development)

```bash
# Build and install the driver to ~/Library/Extensions/
cd driver && make install

# Build the Rust binaries
cargo build --release

# Install the LaunchDaemon
sudo scripts/install-macos.sh target/debug/keymapperd
```

The `make install` target builds a universal binary (arm64 + x86_64) and copies the `.kext` bundle to `~/Library/Extensions/`, where macOS loads it automatically.

### Standalone binaries (DMG)

The release DMG includes the prebuilt driver and all installation scripts. Mount the DMG and run:

```bash
sudo ./install.sh
```

This copies binaries to `/usr/local/bin/`, installs the driver, and registers the LaunchDaemon.

## First-run approval

The first time the DriverKit driver loads, macOS will block it until you approve it:

1. Open **System Settings** > **Privacy & Security**.
2. Scroll to the **Security** section. You will see a message like:
   > "KeyMapperVirtualHID from developer \"(ad-hoc)\" was blocked from loading."
3. Click **Allow**.
4. Reboot (required on macOS Ventura and later for ad-hoc signed extensions).
5. Verify the daemon is running: `launchctl print system/de.adrhinum.keymapperd`.

## Verifying the setup

```bash
# Check that the LaunchDaemon is loaded
launchctl print system/de.adrhinum.keymapperd

# Check that the virtual HID driver is loaded
ioreg -p IOService | grep KeyMapperVirtualHID

# Check seized devices (devices opened by keymapperd disappear from the
# standard event stream)
system_profiler SPUSBDataType | grep -A5 Keyboard
```

You should see output similar to:

```
|   +--o KeyMapperVirtualHID  <class KeyMapperVirtualHID, id..., registered, matched, active>
```

If the device does not appear, check system logs for DriverKit errors:

```bash
log show --predicate 'process == "KeyMapperVirtualHID"' --last 1h
```

## Daemon logs

The daemon writes logs to `/var/log/keymapperd/`:

- `keymapperd.log` — standard output (info-level messages)
- `keymapperd-err.log` — standard error (warnings and errors)

View live logs:

```bash
sudo tail -f /var/log/keymapperd/keymapperd.log
```

## Troubleshooting

### Keyboard appears unresponsive after starting the daemon

**Cause:** The keyboard has been seized by IOKit and its events are being filtered. If the daemon is not running or crashes, seized devices will not forward events.

**Fix:** Restart the daemon:

```bash
sudo launchctl bootout system/de.adrhinum.keymapperd
sudo launchctl bootstrap system /Library/LaunchDaemons/de.adrhinum.keymapperd.plist
```

If the issue persists, check the error log: `sudo cat /var/log/keymapperd/keymapperd-err.log`.

### Driver not loading

**Symptom:** `keymapperd` starts but remapped keys do not produce output, or you see "driver not available" in the logs.

**Fix:**
1. Verify the `.kext` bundle exists at `~/Library/Extensions/KeyMapperVirtualHID.kext`. If missing, rebuild and install: `cd driver && make install`.
2. Check **System Settings** > **Privacy & Security** for a blocked driver prompt. Click **Allow**.
3. Reboot and try again. macOS sometimes requires a full reboot after allowing an ad-hoc signed extension.
4. Check system logs for load failures: `log show --predicate 'process == "KeyMapperVirtualHID"' --last 1h`.

### Remapped keys not working in specific applications

**Symptom:** Key remapping works globally but fails in one or more apps (e.g., terminal emulators, games, Electron apps with raw input).

**Cause:** Some applications use low-level input APIs (e.g., `IOHIDLib` directly) that bypass the standard event stream. Even a hardware-level HID device may not inject events into these apps.

**Fix:** No workaround available. These applications explicitly opt out of system-level input handling. Consider using the application's own key remapping features if available.

### Daemon fails to start

**Symptom:** `launchctl print system/de.adrhinum.keymapperd` shows the service is not loaded.

**Fix:**
1. Check that the binary path in the plist is correct: `cat /Library/LaunchDaemons/de.adrhinum.keymapperd.plist`.
2. Verify the binary is executable: `ls -la /usr/local/bin/keymapperd`.
3. Check error logs: `sudo cat /var/log/keymapperd/keymapperd-err.log`.
4. Reinstall: `sudo ./install-macos.sh /usr/local/bin/keymapperd`.

## Uninstalling

### Remove the daemon and binaries

```bash
sudo ./uninstall-macos.sh
sudo rm /usr/local/bin/keymapper /usr/local/bin/keymapperd
```

### Remove the driver

```bash
# Stop the daemon first
sudo ./uninstall-macos.sh

# Remove the driver bundle
rm -rf ~/Library/Extensions/KeyMapperVirtualHID.kext
```

The driver does not persist across reboots unless the LaunchDaemon is configured to auto-start. After removing both, all keyboards return to normal operation.

## Building the driver (advanced)

The driver is an Xcode project located at `driver/KeyMapperVirtualHID.xcodeproj`. It requires:

- macOS 13.0+ (Ventura) as the host OS.
- Xcode 14+ with command-line tools.

Build commands:

| Target | Command | Description |
|--------|---------|-------------|
| `make build` | Debug, active architecture only | Fast iteration |
| `make release` | Release, universal (arm64 + x86_64) | Distribution build |
| `make install` | Build release + copy to `~/Library/Extensions/` | Full dev install |
| `make clean` | Remove build artifacts | Clean slate |

The driver is ad-hoc signed automatically during the build. No certificate provisioning is required.
