# Windows — low-level hook capture and SendInput emission

keymapperd on Windows uses two in-box mechanisms, with no driver component:

1. **Low-level keyboard hook** (`WH_KEYBOARD_LL`) — a session-global hook that intercepts every key event in the user session.
2. **`SendInput`** — emits remapped key events as synthetic input.

This is the final architecture for Windows: there is no virtual HID driver, and none is planned. The daemon runs as a plain user-mode process — nothing to install, sign, or update, and no elevation is required. The trade-offs of `SendInput` emission are documented in [Limitations](#limitations).

## How it works

### Three-thread architecture

`keymapperd` runs three threads:

1. **Hook thread** — installs the `WH_KEYBOARD_LL` hook (session-global) and runs the `GetMessageW` message loop. For each key event it sends a `HookEvent` to the worker thread and waits for the decision on a one-shot reply channel, polling with 1 ms sleeps. If no decision arrives within about 50 ms, the event is passed through — the input chain must never be blocked.
2. **Raw input thread** — owns a message-only window registered for raw input (`RIDEV_INPUTSINK`) on both keyboard and consumer control devices, so events arrive even when the daemon is not in the foreground. It pumps `WM_INPUT` messages and forwards decoded events to the worker.
3. **Worker thread** — listens on both channels with `crossbeam_channel::select!`. It matches hook events against recent raw input events to identify the source keyboard, performs the mapping lookup, and replies with `swallow` (carrying the resolved outputs) or `pass through`.

The hook procedure itself performs no mapping: the worker decides with device identification, and the hook thread only applies the decision. This avoids a mismatch where the hook would look up without device context.

### Device identification (raw input)

The low-level hook cannot identify the source device — `KBDLLHOOKSTRUCT` carries no device handle. Raw input solves this: each `WM_INPUT` message exposes the source's `hDevice`, which the worker resolves to a device interface path via `GetRawInputDeviceInfoW` (cached per handle). The path format matches the `device` field populated by `keymapper keyboards`, so per-group `keyboards` filters work on Windows.

Matching details:

- Raw input key-downs are buffered with a 100 ms expiry to compensate for non-deterministic arrival order between the hook and raw input streams.
- A hook event matches the most recent buffered raw input event with the same decoded `HidUsage`; the matched entry is consumed.
- If no match arrives within 10 ms, the worker falls back to a lookup without device identification.
- A decision cache keyed by virtual-key code ensures that a key-up is treated consistently with its key-down; emission happens only on key-down.

### Key identity

Compiled rules are keyed by `HidUsage`, not by virtual-key code:

- Hook events and raw keyboard events: the VK is converted via the static `Key` table. VKs without a `HidUsage` (e.g. Print Screen) always pass through.
- Raw HID events (`RIM_TYPEHID`, e.g. media keys from standalone consumer control devices): the raw report is decoded to a `HidUsage` via `hid.dll` (`HidP_GetData`) using the device's report descriptor.

### Modifier tracking

The hook thread maintains the pressed-modifier state from its own event stream rather than `GetAsyncKeyState`: the async state lags behind the very event being processed (fast chords would miss their modifiers) and is poisoned by session leftovers such as a modifier stuck "down" after an interrupted input sequence. Since every key event — physical or injected — passes through the low-level hook, tracking from the events is both faster and exact. The current key's own modifier bit is cleared before lookup so that bare-modifier triggers (e.g. `LeftControl: A`) match correctly.

`GetAsyncKeyState` is used only for standalone consumer control events, which have no hook event to derive the state from.

### Emission and self-exclusion

A mapped output is emitted as a complete tap via `SendInput`: modifiers down (ascending bit order), base key down, base key up, modifiers up (descending), with 1 ms pauses between events and `KEYEVENTF_EXTENDEDKEY` set for extended keys. The output's `HidUsage` is resolved to a virtual-key code — Keyboard page usages through the `Key` table, Consumer page usages through a static translation table (media and volume keys). If an output has no VK equivalent (e.g. brightness keys), the daemon logs an error and releases any modifiers it already pressed, avoiding a stuck-modifier state.

Because the low-level hook is session-global, the daemon's own `SendInput` events reach it. A static set of `(vk_code, is_key_down)` pairs tracks the daemon's active injections; the hook procedure skips and clears them so its own output is never processed as new input.

### Standalone consumer control

Media keys from standalone consumer control devices (e.g. a USB media keypad) do not produce a virtual-key code and therefore never reach the low-level hook. The worker processes their raw input events directly: it performs the lookup and emits the mapped output itself.

The original media action cannot be suppressed: Windows delivers consumer control input to the shell as `WM_APPCOMMAND`, which no keyboard-level hook intercepts. A mapped media key therefore produces both the original action and the remapped output. (Media keys on keyboards that expose a VK code, e.g. `VK_MEDIA_PLAY_PAUSE`, go through the normal hook path and can be swallowed.)

## Limitations

These are accepted trade-offs of the final architecture:

- **Injected events are visible as synthetic.** `SendInput` marks its events (`LLKHF_INJECTED`, `dwExtraInfo`). Applications that filter synthetic input may ignore remapped keys, and — as with any synthetic input — events cannot be delivered to elevated windows (UIPI).
- **Standalone media actions cannot be suppressed** (see [Standalone consumer control](#standalone-consumer-control)).
- **The document-level `keyboards` filter is a no-op.** Capture is a session-global hook; applying the global filter per device is out of scope. Per-group `keyboards` filters work via raw input device identification.

## Capture mode (e2e)

For end-to-end testing, an `e2e` build can be started with the `KEYMAPPER_CAPTURE` environment variable set. In capture mode the daemon swallows every key and re-emits it through `SendInput` tagged with a magic value in `dwExtraInfo`, so the e2e monitor's own `WH_KEYBOARD_LL` hook can capture exactly the daemon's output without depending on a focused window. All emission happens on the worker thread — a `SendInput` issued from within a low-level hook callback does not reach other hooks. Capture mode is compiled out of production builds, so the environment variable has no effect there.

## Source files

| File | Responsibility |
| ---- | -------------- |
| `src/platform/windows/mapping.rs` | Hook thread, emission, self-exclusion, capture mode |
| `src/platform/windows/raw_input.rs` | Raw input window, device identification, HID report decoding |
| `src/platform/windows/dispatch.rs` | Worker thread, event matching, decision cache |
| `src/platform/windows/key.rs` | VK ↔ `HidUsage` conversion, consumer VK table |
| `src/platform/windows/keyboard.rs` | Keyboard enumeration (SetupAPI + HID API) |
| `src/platform/windows/mod.rs` | Module root, capture tag |

## References

- [WH_KEYBOARD_LL](https://learn.microsoft.com/en-us/windows/win32/winmsg/wh-keyboard-ll)
- [SetWindowsHookExW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowshookexw)
- [Raw Input](https://learn.microsoft.com/en-us/windows/win32/inputdev/raw-input)
- [SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)
- [WM_APPCOMMAND](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-appcommand)
