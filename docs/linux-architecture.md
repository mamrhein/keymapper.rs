# Linux — evdev device grab and uinput virtual keyboard

keymapperd on Linux uses two in-kernel mechanisms, with no driver component:

1. **evdev device grab** — captures input by opening each keyboard's `/dev/input/event*` node and grabbing it, so the kernel delivers its events only to the daemon.
2. **uinput virtual keyboard** — emits remapped key events through a virtual input device that the kernel treats as a real physical keyboard.

The daemon runs as an ordinary user, typically via the systemd user service installed by `scripts/install-linux.sh`. No root is required, but the user needs read access to the `/dev/input/event*` nodes (usually through the `input` group) and write access to `/dev/uinput`.

## How it works

### Startup and device management

At startup the daemon:

1. Discovers keyboards via udev (subsystem `input`, property `ID_INPUT_KEYBOARD=1`). Devices that also support absolute (pointer) events are excluded — they are typically touchpads or touchscreens that happen to announce keyboard capabilities.
2. Applies the document-level `keyboards` filter, then grabs each selected device and sets it non-blocking. Grabbing does not flush the device's kernel ring buffer, so the daemon discards every event already buffered there (e.g. the Enter press that started the daemon itself), which would otherwise be re-emitted as a bare key-up without a matching press or a burst of auto-repeat taps. Keys still held at grab time are captured from the kernel's key state (`EVIOCGKEY`) immediately after the drain — the sample must reflect the grab instant, because a key released before a late sample leaves its auto-repeat tail in the ring, and repeats for a key the engine never saw a key-down for are decided as a fresh press (each forwarded as a press+release tap — for the Enter key that started the daemon, a burst of extra newlines). A held modifier is noted as forwarded in the engine (restoring its lookup bit) and its key-down is re-emitted on the virtual device once that exists (idempotent — the client already has the physical press; a modifier released in between is balanced by its forwarded key-up from the ring) so the output key state and the engine's modifier state start in step, while a held non-modifier (e.g. the Enter key that started the daemon, if the press is still in flight) is only marked _stale_ in the engine — re-emitting its key-down, auto-repeat tail, and release would inject a second press (each repeat an extra newline), so the engine swallows its repeats and forwards its release, which balances the pre-grab key-down. Keys the translation table cannot resolve are tracked stale by key code only, since they never reach the engine's decide path. The capture and the stale marking only cover keys held _at_ grab time; a key released between the grab and the first event-loop read simply arrives as a stray key-up, which is forwarded and harmless.
3. Creates the uinput virtual keyboard (`CrossPlatform_Virtual_Keyboard`) with the full evdev key range, and waits 200 ms before continuing.
4. Registers `SIGINT`/`SIGTERM` handlers, adds all grabbed devices to a single epoll instance, and starts the hot-plug monitor.

The daemon degrades gracefully: with no keyboards at startup it runs with an empty managed set, and the hot-plug monitor adopts devices as they appear.

### Event loop

The main thread blocks on `epoll_wait` across all grabbed devices. For each ready device it drains all pending events (non-blocking read) and processes them with that device's own state. The managed device list is shared with the hot-plug thread behind a mutex; the lock is held during per-device processing, and since hot-plug operations are rare the contention is negligible.

### Key identity

Compiled rules are keyed by `HidUsage`, not by evdev key code:

- The kernel emits an `MSC_SCAN` event before the `EV_KEY` event of each key press, carrying the raw HID usage as `(page << 16) | id`. The daemon buffers it and resolves it directly, without any table lookup.
- Key-ups, auto-repeats, and devices that do not emit `MSC_SCAN` fall back to a reverse lookup from the evdev key code through a static translation table.
- Keys with no resolvable HID identity are forwarded unchanged and cannot be mapped.

### Modifier tracking

All bookkeeping — pressed and swallowed keys, forwarded/consumed modifier masks, held output modifiers — lives in the shared mapping engine (`src/daemon/engine.rs`), with one instance per managed device, so one keyboard's modifiers never affect another. The engine maintains the pressed-modifier state from its own event stream: the lookup uses a pre-update snapshot so that bare-modifier triggers (e.g. `LeftControl: A`) match against the concurrent modifier set.

- **Clean tap.** When a trigger fires while an unmapped modifier is held, the engine releases that modifier on the virtual keyboard first and marks it consumed, so the emitted output is not an unintended chord and the modifier's physical release is swallowed rather than forwarded a second time.
- **Recorded key fate.** A key-up's fate is decided from its key-down's own record in the engine, **not** from a re-run of the lookup: the modifier state may have changed between the key-down and the key-up (releasing a modifier is the common case), and re-deriving would leak the release as a phantom key-up, or swallow it while the key-down passed through and leave the key held.
- **Held remapped modifiers.** For a swallowed key-down whose mapped output is itself a modifier key (e.g. `CapsLock: LeftControl`), the engine holds the output's modifier bits down on the virtual keyboard until the physical key-up. The held bits count as active modifiers for subsequent lookups, so chord triggers can match while the remapped modifier is pressed; they are released when the physical key-up arrives, or when a later fired trigger consumes them.

### Mapping and emission

For each key event the daemon looks up the trigger — active-app rules first, then global rules — passing the device path so per-group `keyboards` filters work:

- **Mapped:** the original event is swallowed and each output is emitted through the virtual keyboard. An output whose base is a regular key is a complete tap: modifiers down (ascending bit order), base key press and release, modifiers up in reverse. An output whose base is itself a modifier key is instead held down (the output's modifier bits, then the base modifier, each key-down only) until the physical key-up, so the remapped modifier stays active for the key presses that follow it. Sub-events are spaced apart (20 ms between modifier events, 1 ms around the base key) because windowing backends sample keyboard state once per frame — a tap that fits entirely between two samples is invisible to them. If emission fails, any keys already pressed are released to avoid a stuck state.
- **Unmapped:** the raw event is forwarded unchanged. Auto-repeat (value 2) is emitted as a press+release pair to avoid key-stick on the virtual device.

Mapped modifier keys are swallowed on both press and release: when a trigger fires, the chord's previously forwarded modifiers — and any held modifier outputs of other remapped keys — are released first so the output is emitted as a clean tap, and the physical release of those modifiers is swallowed (see the clean-tap bullet above). A modifier key that fires a trigger is likewise not forwarded: its own bit is cleared from the lookup state at fire time, since it is mapped rather than passed through.

### Self-exclusion

The daemon's own uinput device is also tagged as a keyboard by udev. The hot-plug monitor skips it by name, so the daemon never grabs its own output — grabbing it would feed emitted events back into the input loop and re-emit them indefinitely. Since only grabbed devices are read, the virtual keyboard's events flow to the compositor as usual and never reach the daemon.

### Hot-plug

A background thread listens for udev add/remove events on the input subsystem:

- **Add:** open the device, skip pointer devices and the daemon's own virtual keyboard, apply the global filter, grab, set it non-blocking, discard events already buffered in the kernel ring (as at startup), and register with epoll and the managed list (rolling back if the epoll registration fails).
- **Remove:** drop the device from the managed list (closing the fd releases the grab) and remove it from epoll.
- **Resync:** the startup snapshot and the monitor's `listen()` call are not atomic. A one-time rescan after `listen()` adopts any keyboard that appeared in between, closing that race window.

### Application scoping

The active application is queried per key event through a 100 ms TTL cache. The backend is selected by `$XDG_SESSION_TYPE` (falling back to trying X11 then Wayland when it is unset). Because the query reads these display variables from the process environment on every call, and that environment is fixed at `exec`, the daemon must be started after the compositor has exported `WAYLAND_DISPLAY`, `DISPLAY` and `DBUS_SESSION_BUS_ADDRESS`; the systemd unit is therefore bound to `graphical-session.target` rather than `default.target` (see [Running under systemd](#running-under-systemd)).

- **X11:** read `_NET_ACTIVE_WINDOW` from the root window, then `_NET_WM_PID` from that window.
- **Wayland:** probe compositors in order — KWin (D-Bus `Workspace3.activeWindow`), GNOME Shell (D-Bus `Eval`), COSMIC (the `cosmic-toplevel-info` protocol extension), then wlroots-based and Hyprland compositors (foreign toplevel list).

Wherever a compositor reports the active window's owning PID, it is resolved to its `.desktop` application id by matching the process's executable name — falling back to its command line against the `Exec` paths, which covers sandboxed apps whose binary name differs from the `Exec` key — against the `.desktop` files in `~/.local/share/applications` and `/usr/share/applications`. Backends that report an app id or class directly use it as-is.

If the query fails, the active app is `unknown` and only global rules apply.

## Limitations

These are accepted trade-offs of the architecture:

- **Runtime changes to the global `keyboards:` filter require a restart.** The hot-plug monitor holds the filter from startup, so newly plugged devices are matched against the original value.
- **Auto-repeat is not preserved.** Repeats are forwarded as press+release pairs (see [Mapping and emission](#mapping-and-emission)).
- **Keys without a resolvable HID identity cannot be mapped** (see [Key identity](#key-identity)).
- **Application scoping depends on compositor support.** If the active application cannot be determined, only global rules apply.
- **Application scoping needs the session environment.** The daemon detects the display server from `$XDG_SESSION_TYPE`, `$WAYLAND_DISPLAY`, `$DISPLAY` and `$DBUS_SESSION_BUS_ADDRESS`, read from its own (immutable) environment. The unit binds to `graphical-session.target` so those variables are inherited at startup. As a fallback for a compositor that doesn't propagate its environment, the Wayland backends discover the `wayland-<N>` socket under `$XDG_RUNTIME_DIR` when `$WAYLAND_DISPLAY` is unset; the X11 and D-Bus backends still rely on `$DISPLAY` (defaulting to `:0`) and the session bus at `$XDG_RUNTIME_DIR/bus`, so a compositor without systemd session integration may still need `systemctl --user import-environment`.

## Running under systemd

`keymapperd` runs as a systemd **user** service installed by `scripts/install-linux.sh`. The unit uses `PartOf=graphical-session.target`, `After=graphical-session.target` and `WantedBy=graphical-session.target`:

- `WantedBy` autostarts the daemon as part of the graphical session transaction.
- `After` orders it behind the target, which compositors (GNOME, KDE Plasma, Sway, wlroots, Hyprland) start only after importing the display/session variables — so the daemon inherits a usable environment instead of a bare one.
- `PartOf` stops the daemon when the session goes down.

If focus detection only works after `systemctl --user restart keymapperd`, the compositor started the daemon before exporting its environment; see the Troubleshooting note in the README.

## Logs

keymapperd logs through the `log` facade to stderr, without an embedded timestamp (the journal adds one). When the daemon runs under the systemd user service, journald records it:

```bash
journalctl --user -u keymapperd -f
```

## E2e capture

The end-to-end tests drive a plain production daemon (no test hooks): the harness plants a fixture config, spawns `keymapperd` with its stderr (the log stream) redirected to a temp file, and waits for the readiness line in that stream. For each phase it raises the daemon's log level to `debug` via the control socket, injects the phase's key sequence through a uinput virtual keyboard (which the daemon grabs, so raw injected keys never leak to the active VT), and collects the log window until the expected `emit` lines appear and the stream goes quiescent. The harness then checks the window against the expected model derived from the config: the `emit` sequence must match exactly, every key that must pass through must appear in a `recv` and a `pass` line, and no `ERROR` lines may occur.

Outside CI the same harness runs in local mode: it drives an already-running daemon (never starting or stopping it) and reads its log from the systemd user journal.

## Source files

| File                                    | Responsibility                                                    |
| --------------------------------------- | ----------------------------------------------------------------- |
| `src/platform/linux/mapping/mod.rs`     | Startup, epoll event loop, virtual device creation                |
| `src/platform/linux/mapping/device.rs`  | Per-device state, event processing, emission                      |
| `src/platform/linux/mapping/hotplug.rs` | udev add/remove monitor, startup resync                           |
| `src/platform/linux/mapping/epoll.rs`   | Raw epoll FFI wrapper                                             |
| `src/platform/linux/keyboard.rs`        | Keyboard enumeration via udev                                     |
| `src/platform/linux/hid_translate.rs`   | HID usage ↔ evdev key code translation                            |
| `src/platform/linux/config_dir.rs`      | XDG configuration directory resolution                            |
| `src/common/app_identity/linux/`        | Active application query (X11, Wayland) and `.desktop` resolution |

## References

- [evdev (kernel documentation)](https://docs.kernel.org/input/evdev.html)
- [uinput (kernel documentation)](https://docs.kernel.org/input/uinput.html)
- [udev(7)](https://man7.org/linux/man-pages/man7/udev.7.html)
- [epoll(7)](https://man7.org/linux/man-pages/man7/epoll.7.html)
