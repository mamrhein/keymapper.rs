# macOS DriverKit Virtual HID Driver

keymapper uses a DriverKit-based virtual HID keyboard on macOS to emit synthetic key events. This approach provides reliable, hardware-level event injection that works across all applications and bypasses the restrictions modern macOS places on `CGEvent`-based input.

## How it works

The driver exposes a virtual USB HID keyboard device to the system. When keymapperd remaps a key, it sends a standard USB HID keyboard report to the driver, which the macOS I/O HID stack treats as input from a real physical keyboard. The communication between keymapperd and the driver is handled through a Unix domain socket.

Because the driver is ad-hoc code-signed (no Developer ID certificate required), it must be built and installed locally. macOS will prompt for approval the first time the driver loads.

## Installation

### Via Homebrew

```bash
brew install keymapper
```

The Homebrew formula builds both the Rust binaries and the DriverKit extension from source. Requires Xcode command-line tools.

### From source (development)

```bash
# Build and install the driver to ~/Library/Extensions/
cd driver && make install

# Build the Rust binaries with the driverkit feature enabled
cargo build --release --features driverkit
```

The `make install` target builds a universal binary (arm64 + x86_64) and copies the `.kext` bundle to `~/Library/Extensions/`, where macOS loads it automatically.

### Standalone binaries (DMG)

The release DMG includes the prebuilt driver. After installing the binaries, install the driver:

```bash
# If installed from the DMG, the driver is bundled alongside the binaries.
# keymapperd will discover it automatically if it is in the expected location.
```

## First-run approval

The first time keymapperd starts with the driver loaded, macOS will block the driver until you approve it:

1. Open **System Settings** > **Privacy & Security**.
2. Scroll to the **Security** section. You will see a message like:
   > "KeyMapperVirtualHID from developer \"(ad-hoc)\" was blocked from loading."
3. Click **Allow**.
4. Restart keymapperd (`keymapper server start`).

After approval, the driver loads automatically on subsequent starts. On macOS Ventura and later, this approval may require a reboot to take full effect.

## Verifying the driver is loaded

```bash
# Check that the virtual HID device exists
ioreg -p IOService | grep KeyMapperVirtualHID
```

You should see output similar to:

```
|   +--o KeyMapperVirtualHID  <class KeyMapperVirtualHID, id..., registered, matched, active>
```

If the device does not appear, check system logs for DriverKit errors:

```bash
log show --predicate 'process == "KeyMapperVirtualHID"' --last 1h
```

## Troubleshooting

### Driver not loading

**Symptom:** keymapperd starts but remapped keys do not produce output, or you see "driver not available" in the logs.

**Fix:**
1. Verify the `.kext` bundle exists at `~/Library/Extensions/KeyMapperVirtualHID.kext`. If missing, rebuild and install: `cd driver && make install`.
2. Check **System Settings** > **Privacy & Security** for a blocked driver prompt. Click **Allow**.
3. Reboot and try again. macOS sometimes requires a full reboot after allowing an ad-hoc signed extension.
4. Check system logs for load failures: `log show --predicate 'process == "KeyMapperVirtualHID"' --last 1h`.

### Remapped keys not working in specific applications

**Symptom:** key remapping works globally but fails in one or more apps (e.g., terminal emulators, games, Electron apps with raw input).

**Cause:** Some applications use low-level input APIs (e.g., `IOHIDLib` directly) that bypass the standard event stream. Even a hardware-level HID device may not inject events into these apps.

**Fix:** No workaround available. These applications explicitly opt out of system-level input handling. Consider using the application's own key remapping features if available.

### CGEvent fallback mode

If the driver is not available, keymapperd falls back to `CGEvent`-based event injection. This mode:

- Requires **Accessibility** permission in System Settings > Privacy & Security > Accessibility.
- May not work in all applications, especially those with input monitoring or anti-cheat protections.
- Is detected and logged automatically at startup.

To enable the driver, follow the installation steps above. To force CGEvent fallback (for debugging), start keymapperd without the `driverkit` feature.

## Uninstalling the driver

```bash
# Stop the daemon
keymapper server stop

# Remove the driver bundle
rm -rf ~/Library/Extensions/KeyMapperVirtualHID.kext
```

The driver does not persist across reboots unless keymapperd is configured to auto-start (e.g., via launchd or Homebrew service).

## Building the driver (advanced)

The driver is an Xcode project located at `driver/KeyMapperVirtualHID.xcodeproj`. It requires:

- macOS 13.0+ (Ventura) as the host OS.
- Xcode 14+ with command-line tools.

Build commands:

| Target | Command | Description |
|--------|---------|-------------|
| `make build` | Debug, active architecture only | Fast iteration |
| `make release` | Release, universal (arm64 + x86_64) | Distribution build |
| `make install` | Build release + copy to `~/Library/Application Support/keymapper/` | Full dev install |
| `make clean` | Remove build artifacts | Clean slate |

The driver is ad-hoc signed automatically during the build. No certificate provisioning is required.
