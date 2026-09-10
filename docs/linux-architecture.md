# Linux — evdev device grab and uinput virtual keyboard

keymapperd on Linux uses two in-kernel mechanisms, with no driver component:

1. **evdev device grab** — captures input by opening each keyboard's `/dev/input/event*` node and grabbing it, so the kernel delivers its events only to the daemon.
2. **uinput virtual keyboard** — emits remapped key events through a virtual input device that the kernel treats as a real physical keyboard.

The daemon runs as an ordinary user, typically via the systemd user service installed by `scripts/install-linux.sh`. No root is required, but the user needs read access to the `/dev/input/event*` nodes (usually through the `input` group) and write access to `/dev/uinput`.

## How it works

### Startup and device management

At startup the daemon:

1. Discovers keyboards via udev (subsystem `input`, property `ID_INPUT_KEYBOARD=1`). Devices that also support absolute (pointer) events are excluded — they are typically touchpads or touchscreens that happen to announce keyboard capabilities.
2. Applies the document-level `keyboards` filter, then grabs each selected device and sets it non-blocking.
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

- **Add:** open the device, skip pointer devices and the daemon's own virtual keyboard, apply the global filter, grab, and register with epoll and the managed list (rolling back if the epoll registration fails).
- **Remove:** drop the device from the managed list (closing the fd releases the grab) and remove it from epoll.
- **Resync:** the startup snapshot and the monitor's `listen()` call are not atomic. A one-time rescan after `listen()` adopts any keyboard that appeared in between, closing that race window.

### Application scoping

The active application is queried per key event through a 100 ms TTL cache. The backend is selected by `$XDG_SESSION_TYPE`:

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

## E2e capture

For end-to-end testing, the e2e monitor (a separate process) does not create a GUI window — whose focus is controlled by the window manager and can be stolen at any time. Instead, it locates the daemon's uinput device by scanning `/sys/class/input` for the device name and grabs it, logging the raw key events the daemon emits. This makes the capture deterministic and headless-friendly, and guarantees the daemon's output never leaks into the compositor or any focused window.

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
