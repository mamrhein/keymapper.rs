# macOS — CGEventTap Input Capture and DriverKit Output

keymapper runs two processes on macOS:

1. **`keymapperd`** — a user-domain LaunchAgent that captures keyboard events with a `CGEventTap`, decides which keys are mapped, and swallows the mapped ones.
2. **`virtkbdd`** — a root LaunchDaemon that emits the mapped keys through the [Karabiner DriverKit VirtualHIDDevice](https://github.com/pqrs-org/Karabiner-DriverKit-VirtualHIDDevice) package (pqrs), which exposes a virtual USB HID keyboard device that the macOS I/O HID stack treats as input from a real physical keyboard.

Unmapped keys pass through the native input path untouched; only mapped keys are swallowed and re-emitted via the virtual keyboard. Because the virtual keyboard is a hardware-level HID device, mapped output works in all applications — including those that use raw input APIs.

## How it works

### Input capture (CGEventTap in keymapperd)

`keymapperd` runs as the logged-in user (a LaunchAgent in the `gui/<UID>` domain). At startup it creates a `CGEventTap` at the HID event tap location (`kCGHIDEventTap`), placed before other taps, listening for key-down, key-up, and flags-changed events.

For every event, the tap callback decides synchronously:

- **Unmapped keys** are returned to the system and pass through natively — no re-emission, no IPC.
- **Mapped keys** are swallowed (the callback returns `NULL`) and the mapped output is sent to virtkbdd asynchronously.

The tap callback must return immediately (the system disables taps that block), so the decision is made locally in keymapperd; see [the decision core](#the-decision-core). If the system disables the tap (`TapDisabledByTimeout` or `TapDisabledByUserInput`), keymapperd re-enables it.

Creating the tap requires two TCC permissions for `keymapperd`: **Input Monitoring** (to see keyboard events) and **Accessibility** (to swallow them). If either is missing, tap creation fails with an actionable error in the log.

### The decision core

The mapping decision lives in a platform-agnostic, unit-tested core (`src/daemon/decision.rs`). For each key event it produces one of three outcomes:

- **Pass** — return the event unchanged (unmapped keys, consumer-page media keys, and the Fn/Caps Lock modifiers).
- **Emit** — swallow the event and send the mapped output to virtkbdd.
- **Swallow** — drop the event without emitting (e.g., auto-repeat of a mapped key).

Emission is fire-and-forget: the output batch is pushed into a bounded channel with `try_send`, and a batch is dropped if the channel is full (the tap callback never blocks).

Because CGEvents do not expose the originating device's location ID, lookups pass `device_id = None` and per-keyboard filters are skipped on macOS (see [known limitations](#known-limitations)).

### Output emission (virtkbdd + Karabiner DriverKit)

`virtkbdd` runs as root (a LaunchDaemon in the `system` domain). At startup it connects to the Karabiner DriverKit daemon over its UNIX stream socket (`/Library/Application Support/org.pqrs/tmp/rootonly/karabiner_virtual_hid_device_service.sock`) using the output keyboard's identity, waits up to 25 seconds for the virtual keyboard to become ready, and retries in the background if the connection is lost.

For each batch it receives from keymapperd, virtkbdd turns every mapped output into a sequence of self-contained `keyboard_input` reports (each modifier down, the base key down/up, each modifier up) and posts them to the virtual keyboard. The macOS I/O HID stack treats this as input from a physical keyboard, so it works in all applications.

The Karabiner daemon is registered as a LaunchDaemon (`/Library/LaunchDaemons/org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon.plist`) with `KeepAlive`, so it restarts automatically if it exits.

virtkbdd has no configuration of its own; all mapping logic lives in keymapperd.

### IPC between the two processes

keymapperd and virtkbdd communicate over a UNIX stream socket at `/var/run/virtkbdd/keymapperd.sock`. The directory is created with mode 0755; the socket itself is chowned to the console user's UID and has mode 0600.

virtkbdd verifies the peer of every connection with `getpeereid(2)` and accepts only the current console user (re-resolved on each accept, so fast user switching works). It serves one connection at a time; keymapperd reconnects itself if the connection drops.

Framing: `[u8 version = 1][u32 LE payload length][payload]`, where the payload is `[u32 LE key count][key count × (u8 modifiers, u32 LE HID code)]` with `HID code = (usage page << 16) | usage ID`. A frame carries at most 64 keys, and payloads larger than 4096 bytes are rejected.

**Failure mode:** if virtkbdd is unreachable, keymapperd marks it as unavailable and every key passes through natively — typing keeps working, only the mappings are inactive. When virtkbdd comes back, keymapperd reconnects and mappings resume without a restart.

### Configuration resolution

keymapperd runs as the console user, so it reads `~/Library/Application Support/keymapperd/` directly — the same file the unprivileged `keymapper` CLI edits. (Older releases ran the daemon as root and had to bridge from `/var/root` to the console user's home directory; that mismatch is gone.)

### Why this design?

The previous architecture captured input by seizing the physical keyboard with IOKit (`IOHIDDeviceOpen` with `kIOHIDOptionsTypeSeizeDevice`) and re-emitted every key through the virtual keyboard. That had a fatal flaw: IOKit seizure is exclusive **among userspace clients only**. It does not detach the kernel `IOHIDEventDriver`, so the native input path stayed alive and every key reached applications twice — natively, plus via the virtual-keyboard re-emission. No userspace API detaches the built-in event driver, and the Karabiner DriverKit virtual keyboard is output-only (it cannot capture).

The current design therefore captures at the CG level instead: a `CGEventTap` swallows mapped keys and lets unmapped keys pass natively, so nothing is ever re-emitted that the system has not already delivered. Output stays on the Karabiner DriverKit virtual keyboard, which is hardware-level and works everywhere.

Two processes are required because the two halves live in security domains that cannot be merged:

- A `CGEventTap` needs a WindowServer connection → **user domain only**. A root LaunchDaemon cannot create one.
- The Karabiner VHK socket is **root-only** → the emitter must be a root system-domain process.

The decision logic lives in keymapperd (user domain) because the tap callback is synchronous: it must return the event (pass) or `NULL` (swallow) immediately in the keyboard delivery path. An IPC round-trip per keystroke to learn "is this mapped?" would add input lag, and the system disables taps that block. So keymapperd decides locally and fast; IPC carries only the asynchronous "emit" of mapped keys, fire-and-forget.

## Installation

### Via Homebrew

```bash
brew install keymapper
```

This builds the Rust binaries from source and installs the Karabiner DriverKit VirtualHIDDevice driver (via a cask dependency). The driver setup requires sudo.

The driver must be enabled in System Settings > General > Login Items & Extensions > Driver Extensions on first run. No reboot is required.

Start the service:

```bash
brew services start keymapper
```

### From source (development)

```bash
# Build and install the Rust binaries
cargo install --path .

# Install both launchd services and the Karabiner DriverKit driver
sudo scripts/install-macos.sh
```

The install script registers the virtkbdd LaunchDaemon (`/usr/local/bin/virtkbdd`), registers the keymapperd LaunchAgent (`~/.local/bin/keymapperd`) for the console user, then installs the pinned Karabiner DriverKit package (downloading it from the pqrs GitHub releases if no local copy is available), activates the DriverKit extension, and registers the Karabiner daemon LaunchDaemon. It is idempotent — safe to run multiple times.

The script also accepts explicit paths: `sudo scripts/install-macos.sh [keymapperd_path] [virtkbdd_path] [karabiner_pkg_path]`.

### Standalone binaries (DMG)

The release DMG includes the pinned Karabiner DriverKit package and all installation scripts. Mount the DMG and run:

```bash
sudo ./install.sh
```

This copies the CLI to `/usr/local/bin/`, installs virtkbdd to `/usr/local/bin/virtkbdd` and keymapperd to `~/.local/bin/keymapperd`, installs the driver, and registers both launchd services.

After installing by any method, grant keymapperd the required privacy permissions (see [first-run approval](#first-run-approval)) and run `keymapper daemon restart`.

## First-run approval

Two things need your attention on first run:

1. **The Karabiner DriverKit extension.** The first time it loads, macOS may require you to enable it:
   1. Open **System Settings** > **General** > **Login Items & Extensions**.
   2. Select **Driver Extensions**.
   3. Toggle on the Karabiner entry (`org.pqrs.Karabiner-DriverKit-VirtualHIDDevice`).

   No reboot is required.

2. **TCC permissions for keymapperd.** Grant both:
   1. **System Settings** > **Privacy & Security** > **Input Monitoring** — enable `keymapperd` (required to see keyboard events).
   2. **System Settings** > **Privacy & Security** > **Accessibility** — enable `keymapperd` (required to swallow mapped keys).

   Then restart the services: `keymapper daemon restart`.

## Verifying the setup

```bash
# Check both keymapper services at once
keymapper daemon status

# Check that the keymapperd LaunchAgent is loaded (gui/<UID> domain)
launchctl print gui/$(id -u)/de.adrhinum.keymapperd

# Check that the virtkbdd LaunchDaemon is loaded (system domain, requires sudo)
sudo launchctl print system/de.adrhinum.virtkbdd

# Check that the Karabiner daemon LaunchDaemon is loaded
launchctl print system/org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon

# Check that the DriverKit extension is enabled (requires sudo)
sudo systemextensionsctl list | grep Karabiner
```

You should see output similar to:

```
... org.pqrs.Karabiner-DriverKit-VirtualHIDDevice ... activated enabled
```

If the extension is not enabled, check system logs for DriverKit errors:

```bash
log show --predicate 'subsystem == "com.apple.systemextensions"' --last 1h
```

## Daemon logs

keymapperd (user domain) writes logs to `~/Library/Logs/keymapper/`:

- `keymapperd.log` — standard output (info-level messages)
- `keymapperd-err.log` — standard error (warnings and errors)

virtkbdd (system domain) writes logs to `/var/log/virtkbdd/`:

- `virtkbdd.log` — standard output (info-level messages)
- `virtkbdd-err.log` — standard error (warnings and errors)

View live logs:

```bash
tail -f ~/Library/Logs/keymapper/keymapperd.log
sudo tail -f /var/log/virtkbdd/virtkbdd.log
```

## Troubleshooting

### Remapping not working, tap error in the log

**Symptom:** keys are not remapped and `~/Library/Logs/keymapper/keymapperd-err.log` reports a CGEventTap creation failure.

**Cause:** the Input Monitoring or Accessibility permission for `keymapperd` is missing or stale.

**Fix:** grant both permissions in **System Settings** > **Privacy & Security**, then restart the services:

```bash
keymapper daemon restart
```

Note that under ad-hoc signing (development builds), the TCC grant is invalidated on every rebuild — re-grant after each `cargo install`.

### Mappings inactive but typing works

**Symptom:** the keyboard types normally, but no remapping happens.

**Cause:** virtkbdd (or the Karabiner DriverKit virtual keyboard) is down. keymapperd passes every key through natively when it cannot reach virtkbdd, so typing keeps working while mappings are inactive.

**Fix:** check `sudo cat /var/log/virtkbdd/virtkbdd-err.log`, then restart the services:

```bash
keymapper daemon restart
```

Mappings resume as soon as keymapperd reconnects — there is no need to restart keymapperd alone.

### Driver not loading

**Symptom:** virtkbdd starts but remapped keys do not produce output, or you see "Karabiner virtual keyboard not ready" in its log.

**Fix:**
1. Verify the extension is enabled: `sudo systemextensionsctl list | grep Karabiner` should show `activated enabled`. If it shows `activated disabled`, enable it in **System Settings** > **General** > **Login Items & Extensions** > **Driver Extensions**.
2. Verify the Karabiner daemon is running: `launchctl print system/org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon`.
3. Re-run the installer to repair the setup: `sudo scripts/install-karabiner-macos.sh`.
4. Check system logs for load failures: `log show --predicate 'subsystem == "com.apple.systemextensions"' --last 1h`.

### Remapped keys not working in specific applications

**Symptom:** key remapping works globally but fails in one or more apps (e.g., terminal emulators, games, Electron apps with raw input).

**Cause:** some applications use low-level input APIs (e.g., `IOHIDLib` directly) that bypass the standard event stream. Even a hardware-level HID device may not inject events into these apps.

**Fix:** no workaround available. These applications explicitly opt out of system-level input handling. Consider using the application's own key remapping features if available.

### A service fails to start

**Symptom:** `keymapper daemon status` reports a service as not running.

**Fix:**
1. Check that the binary path in the plist is correct: `cat ~/Library/LaunchAgents/de.adrhinum.keymapperd.plist` and `sudo cat /Library/LaunchDaemons/de.adrhinum.virtkbdd.plist`.
2. Verify the binaries are executable: `ls -la ~/.local/bin/keymapperd /usr/local/bin/virtkbdd`.
3. Check the error logs: `cat ~/Library/Logs/keymapper/keymapperd-err.log` and `sudo cat /var/log/virtkbdd/virtkbdd-err.log`.
4. Reinstall: `sudo scripts/install-macos.sh`.

## Known limitations

- **Keyboard filtering is disabled on macOS.** CGEvents do not expose the originating device's location ID, so lookups pass `device_id = None` and global and per-rule `keyboards:` filters are skipped.
- **Modifier-trigger semantics.** Trigger modifiers pass through natively and remain held when the virtual-keyboard output arrives. `Shift+Backspace → Delete` may be seen by applications as `Shift+Delete` (usually harmless on macOS, where Shift+Delete behaves like Delete, but app-dependent).
- **Consumer-page (media) keys pass through** and cannot be triggers.
- **Auto-repeat of mapped keys is swallowed.** One virtual-keyboard tap per physical press.
- **TCC re-grant on every rebuild** under ad-hoc signing (development builds).

## Uninstalling

```bash
sudo ./uninstall-macos.sh
sudo rm /usr/local/bin/keymapper ~/.local/bin/keymapperd /usr/local/bin/virtkbdd
```

`uninstall-macos.sh` stops and removes the keymapperd LaunchAgent and the virtkbdd LaunchDaemon (including a legacy keymapperd LaunchDaemon left over from older releases), deactivates the Karabiner DriverKit extension, and removes the Karabiner package files (including its daemon LaunchDaemon). It does not delete log files. After removing both services, all keyboards return to normal operation.
