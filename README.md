# keymapperd

Cross-platform key-remapping daemon for macOS, Linux, and Windows. Intercepts keyboard events and remaps them based on a TOML configuration file, with per-application scoping and hot-reload.

## Installation

Requires a recent Rust toolchain.

```bash
cargo install --path .
```

Run with appropriate privileges for keyboard interception (Accessibility on macOS, `/dev/input` access on Linux).

## Configuration

Create `config.toml` in one of the following locations:

| Platform | Path |
|----------|------|
| Linux | `$XDG_CONFIG_HOME/keymapperd/config.toml` (defaults to `~/.config/keymapperd/`) |
| macOS | `~/Library/Application Support/keymapperd/config.toml` |
| Windows | `%APPDATA%\keymapperd\config.toml` |

The daemon exits if no configuration file is found.

### Format

```toml
[[rules]]
description = "Remap CapsLock to LeftControl globally"
trigger = "CapsLock"
action = { RemapTo = "LeftControl" }
applications = []

[[rules]]
description = "Map F1 to Cmd+T in Chrome"
trigger = "F1"
action = { Shortcut = ["leftcommand", "t"] }
applications = ["Google Chrome"]
```

### Rule fields

| Field | Required | Description |
|-------|----------|-------------|
| `description` | No | Human-readable comment (ignored at runtime) |
| `trigger` | Yes | The key to intercept |
| `action` | Yes | What to do when the trigger fires |
| `applications` | Yes | List of application names to scope the rule. Empty list (`[]`) means global |

### Actions

| Action | Description | Example |
|--------|-------------|---------|
| `RemapTo` | Replace the key with another single key | `{ RemapTo = "LeftControl" }` |
| `Shortcut` | Emit a sequence of key events | `{ Shortcut = ["leftcommand", "t"] }` |

### Key names

All key names are case-insensitive. Recognised keys include:

- **Modifiers:** `leftcontrol`, `rightcontrol`, `leftshift`, `rightshift`, `leftalt`, `rightalt`, `leftcommand`, `rightcommand`, `capslock`
- **Navigation:** `tab`, `space`, `return`, `backspace`, `delete`, `escape`, `uparrow`, `downarrow`, `leftarrow`, `rightarrow`, `pageup`, `pagedown`, `home`, `end`
- **Function keys:** `f1` through `f12`
- **Letters:** `a` through `z`
- **Numbers:** `0` through `9`

### Common aliases

The following aliases resolve to their canonical key name:

| Alias | Resolves to |
|-------|-------------|
| `ctrl`, `leftctrl` | `leftcontrol` |
| `rightctrl` | `rightcontrol` |
| `shift`, `leftshift` | `leftshift` |
| `alt`, `leftalt` | `leftalt` |
| `cmd`, `command`, `super` | `leftcommand` |
| `rightcmd`, `rightcommand` | `rightcommand` |
| `caps` | `capslock` |
| `enter` | `return` |
| `esc` | `escape` |
| `up`, `uparrow`, `up_arrow` | `uparrow` |
| `down`, `left`, `right` | (direction + `arrow`) |
| `pgup`, `pgdn` | `pageup`, `pagedown` |

## Hot-reload

Edit and save your `config.toml` while the daemon is running. Changes take effect immediately without restarting. Invalid configurations are rejected and the previous configuration is retained.

## How it works

| Platform | Mechanism |
|----------|-----------|
| Linux | `evdev` device grab + `uinput` virtual keyboard |
| macOS | `CGEventTap` (requires Accessibility permission) |
| Windows | Low-level keyboard hook (`WH_KEYBOARD_LL`) |
