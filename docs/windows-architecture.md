# Windows — low-level hook capture and SendInput emission

keymapperd on Windows uses two in-box mechanisms, with no driver component:

1. **Low-level keyboard hook** (`WH_KEYBOARD_LL`) — a session-global hook that intercepts every key event in the user session.
2. **`SendInput`** — emits remapped key events as synthetic input.

This is the final architecture for Windows: there is no virtual HID driver, and none is planned. The daemon runs as a plain user-mode process — nothing to install, sign, or update, and no elevation is required. The trade-offs of `SendInput` emission are documented in [Limitations](#limitations).

## How it works

### Two-thread architecture

`keymapperd` runs two threads:

1. **Hook thread** — installs the `WH_KEYBOARD_LL` hook (session-global) and runs the message loop. The hook procedure performs the entire decision and emission itself: it matches the event against the raw input buffer for device identification (non-blocking, with a bounded 3 ms retry), asks the shared mapping engine for the event's fate, and either emits the mapped output via `SendInput` and swallows the key, or passes the key through. A key-up is decided from its key-down's own record in the engine, not from a re-run of the lookup.

   The message loop exists only to keep the hook alive and to drain the deferred-emission queue (fed exclusively by standalone consumer events, see [Standalone consumer control](#standalone-consumer-control)): the hook callback runs re-entrantly inside the blocked message wait, and a swallowed hook event produces no message of its own — a bare `GetMessageW` loop would block forever and never run its body. The loop therefore blocks in `MsgWaitForMultipleObjects` on the input queue, drains the queue with the non-blocking `PeekMessageW`, and then runs the emission drain.
2. **Raw input thread** — owns a message-only window registered for raw input (`RIDEV_INPUTSINK`) on both keyboard and consumer control devices, so events arrive even when the daemon is not in the foreground. It pumps `WM_INPUT` messages, buffers keyboard key-downs in the shared device-identification buffer, and processes standalone consumer events directly (see [Standalone consumer control](#standalone-consumer-control)).

### Device identification (raw input)

The low-level hook cannot identify the source device — `KBDLLHOOKSTRUCT` carries no device handle. Raw input solves this: each `WM_INPUT` message exposes the source's `hDevice`, which is resolved to a device interface path via `GetRawInputDeviceInfoW` (cached per handle in a process-wide cache shared by both threads). The path format matches the `device` field populated by `keymapper keyboards`, so per-group `keyboards` filters work on Windows.

Matching details:

- Raw input key-downs are buffered with a 100 ms expiry to compensate for non-deterministic arrival order between the hook and raw input streams. Key-ups carry no new device information and are dropped.
- The hook procedure matches its event against the most recent buffered raw input event with the same decoded `HidUsage`; the matched entry is consumed so it is never reused for a subsequent press.
- The match is non-blocking except for a bounded retry (a 3 ms budget, 1 ms sleeps), long enough for the raw event of the same press to arrive in the common case and short enough to stay well inside Windows' low-level-hook timeout.
- A press that never matches degrades to a lookup without device identification: device-filtered rules simply do not fire for that event, and the input chain is never blocked for long.

### Key identity

Compiled rules are keyed by `HidUsage`, not by virtual-key code:

- Hook events and raw keyboard events: the VK is converted via the static `Key` table. VKs without a `HidUsage` (e.g. Print Screen) always pass through.
- Raw HID events (`RIM_TYPEHID`, e.g. media keys from standalone consumer control devices): the raw report is decoded to a `HidUsage` via `hid.dll` (`HidP_GetData`) using the device's report descriptor.

### Modifier tracking

All bookkeeping — pressed and swallowed keys, forwarded/consumed modifier masks, held output modifiers — lives in the shared mapping engine (`src/daemon/engine.rs`), which the hook procedure asks on every event. The engine maintains the pressed-modifier state from its own event stream rather than `GetAsyncKeyState`: the async state lags behind the very event being processed (fast chords would miss their modifiers) and is poisoned by session leftovers such as a modifier stuck "down" after an interrupted input sequence. Since every key event — physical or injected — passes through the low-level hook, tracking from the events is both faster and exact. The current key's own modifier bit is cleared before lookup so that bare-modifier triggers (e.g. `LeftControl: A`) match correctly.

Two consequences of the shared bookkeeping:

- **Clean tap.** When a trigger fires while an unmapped modifier is held, the engine releases that modifier on the output side first (a tagged key-up) and marks it consumed, so the emitted output is not an unintended chord and the modifier's physical release is swallowed.
- **Held remapped modifiers.** An output whose base is itself a modifier key (e.g. `CapsLock: LeftControl`) is held down on the output side until the physical key-up, so the remapped modifier stays active for subsequent key presses.

`GetAsyncKeyState` is used only for standalone consumer control events, which have no hook event to derive the state from.

### Emission and self-exclusion

The `SendInput` is performed by the hook procedure directly in the callback. A `SendInput` issued from within a `WH_KEYBOARD_LL` callback reaches other hooks and the target window — the e2e monitor captures the tagged re-emission through a separate process's hook — so the previous design (worker thread, one-shot reply channel, deferred emission) is not load-bearing. The deferred queue remains only for standalone consumer events, whose emission originates on the raw input thread: a `SendInput` issued there can race a keyboard hook chain in progress and be dropped, so those outputs are queued and posted to the message loop, which drains them where the hook chain is idle.

A mapped output is emitted as a complete tap via `SendInput`: modifiers down (ascending bit order), base key down, base key up, modifiers up (descending), with 1 ms pauses between events and `KEYEVENTF_EXTENDEDKEY` set for extended keys. An output whose base is itself a modifier key is the exception: only its key-downs are sent, and the matching key-ups go out when the physical key-up arrives (see [Modifier tracking](#modifier-tracking)). The output's `HidUsage` is resolved to a virtual-key code — Keyboard page usages through the `Key` table, Consumer page usages through a static translation table (media and volume keys). If an output has no VK equivalent (e.g. brightness keys), the daemon logs an error and releases any modifiers it already pressed, avoiding a stuck-modifier state.

Because the low-level hook is session-global, the daemon's own `SendInput` events reach it. Every injected event is stamped with a magic value in `dwExtraInfo` (`INJECTED_TAG`), and the hook procedure passes tagged events through without re-mapping, so its own output is never processed as new input. Matching on the tag is exact: a physical press of the same key can never be mistaken for one of the daemon's injections.

### Standalone consumer control

Media keys from standalone consumer control devices (e.g. a USB media keypad) do not produce a virtual-key code and therefore never reach the low-level hook. The raw input thread processes their raw input events directly: it performs the lookup and queues the mapped output for the message loop, so standalone consumer emission is subject to the same delivery constraints as any emission that cannot run in the hook callback.

The original media action cannot be suppressed: Windows delivers consumer control input to the shell as `WM_APPCOMMAND`, which no keyboard-level hook intercepts. A mapped media key therefore produces both the original action and the remapped output. (Media keys on keyboards that expose a VK code, e.g. `VK_MEDIA_PLAY_PAUSE`, go through the normal hook path and can be swallowed.)

## Limitations

These are accepted trade-offs of the final architecture:

- **Injected events are visible as synthetic.** `SendInput` marks its events (`LLKHF_INJECTED`, `dwExtraInfo`). Applications that filter synthetic input may ignore remapped keys, and — as with any synthetic input — events cannot be delivered to elevated windows (UIPI).
- **Standalone media actions cannot be suppressed** (see [Standalone consumer control](#standalone-consumer-control)).
- **The document-level `keyboards` filter is a no-op.** Capture is a session-global hook; applying the global filter per device is out of scope. Per-group `keyboards` filters work via raw input device identification.

## E2e capture

For end-to-end testing, the e2e monitor installs its own `WH_KEYBOARD_LL` hook in a separate process and captures every key event that reaches the session's hook chain. Because the daemon's hook is installed first, it swallows remapped inputs before the monitor sees them, so the monitor observes exactly the daemon's tagged outputs plus forwarded passthroughs — never the raw injected inputs.

## Source files

| File | Responsibility |
| ---- | -------------- |
| `src/platform/windows/mapping.rs` | Hook thread, engine decision, emission, self-exclusion |
| `src/platform/windows/raw_input.rs` | Raw input window, HID report decoding |
| `src/platform/windows/raw_worker.rs` | Raw input thread, standalone consumer events |
| `src/platform/windows/device_match.rs` | Device-identification buffer, device path cache |
| `src/platform/windows/key.rs` | VK ↔ `HidUsage` conversion, consumer VK table |
| `src/platform/windows/keyboard.rs` | Keyboard enumeration (SetupAPI + HID API) |
| `src/platform/windows/mod.rs` | Module root, injection tag |

## References

- [WH_KEYBOARD_LL](https://learn.microsoft.com/en-us/windows/win32/winmsg/wh-keyboard-ll)
- [SetWindowsHookExW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowshookexw)
- [Raw Input](https://learn.microsoft.com/en-us/windows/win32/inputdev/raw-input)
- [SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)
- [WM_APPCOMMAND](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-appcommand)
