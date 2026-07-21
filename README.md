# keymapper

Cross-platform key-remapping daemon and CLI utility for macOS, Linux, and Windows. Intercepts keyboard events and remaps them based on a YAML configuration file, with per-application scoping, chord (modifier + key) triggers and outputs, hot-reload, and macros.

The project ships two binaries:

- **`keymapperd`** — the background daemon that intercepts keyboard events and applies remapping rules.
- **`keymapper`** — a CLI utility for managing configuration, inspecting keys, and controlling the daemon.

## Installation

Requires Rust 1.88+ (edition 2024).

```bash
cargo install --path .
```

Run `keymapperd` with appropriate privileges for keyboard interception (Accessibility on macOS, `/dev/input` access on Linux).

## Quick start

```bash
# Create an empty configuration file
keymapper config create

# List visible applications (for scoping rules)
keymapper appnames

# Add a mapping rule
keymapper config add CapsLock LeftControl

# Validate your configuration
keymapper config check

# Start the daemon
keymapper server start
```

## Configuration

Create `config.yaml` in one of the following locations:

| Platform | Path |
|----------|------|
| Linux | `$XDG_CONFIG_HOME/keymapperd/config.yaml` (defaults to `~/.config/keymapperd/`) |
| macOS | `~/Library/Application Support/keymapperd/config.yaml` |
| Windows | `%APPDATA%\keymapperd\config.yaml` |
| Any | Current working directory (development convenience) |

Search order is CWD first, then the platform-specific application config directory. Symbolic links are rejected; `config.yaml` must be a regular file.

The daemon exits with an error if no configuration file is found in any search location.

### Format

```yaml
# Global: swap CapsLock and LeftControl
- mappings:
    CapsLock: LeftControl
    LeftControl: CapsLock

# Vim-style navigation in iTerm2
- name: "iterm nav"
  apps: [iTerm2]
  mappings:
    Ctrl+H: Left
    Ctrl+J: Down
    Ctrl+K: Up
    Ctrl+L: Right

# Global chord shortcuts — outputs are real chords, not sequential presses
- name: "workspace switch"
  mappings:
    Ctrl+Shift+Left: Cmd+Left
    Ctrl+Shift+Right: Cmd+Right

# Modifier remapping — emit LeftAlt+L when pressing RightAlt
- mappings:
    RightAlt: LeftAlt+L

# Macro — emit a sequence of key events
- mappings:
    F1: [Cmd+C, T]
```

### Structure

The document is a YAML sequence of rule groups. Each group has:

| Field | Required | Description |
|-------|----------|-------------|
| `name` | No | Human-readable label (ignored at runtime) |
| `apps` | No | List of application names to scope the group. Omit or leave empty for global rules |
| `mappings` | Yes | Key-value pairs mapping triggers to outputs |

Groups are evaluated in definition order. Within each group, mappings are evaluated top-to-bottom; the first matching trigger wins.

### Mappings

Each mapping is a `trigger: output` pair inside a `mappings:` block.

| Output | Description | Example |
|--------|-------------|---------|
| Single key or chord string | Replace the trigger with one key event (modifiers held while pressing base) | `CapsLock: LeftControl` |
| List of chord strings | Emit a sequence of key events (macro) | `F1: [Cmd+C, T]` |

Every output is a **chord**: modifier keys are held while the base key is pressed, then released in reverse. For example, `Cmd+C` is emitted as "press Cmd → press C → release C → release Cmd", ensuring the modifier has its intended effect.

### Triggers

Triggers use compact `+`-separated strings. The last token is the base key; all preceding tokens are modifiers.

| Syntax | Example | Meaning |
|--------|---------|---------|
| Bare key | `CapsLock` | Single key with no modifier requirement |
| Modifier + key | `Ctrl+H` | Ctrl held while pressing H |
| Multiple modifiers | `Cmd+Shift+T` | Cmd + Shift held while pressing T |

**Modifier resolution:** generic modifier names resolve to their left-side default. `Ctrl` becomes left Control, `Alt` becomes left Alt, `Cmd` becomes left Command, and so on. Use the explicit names (`LeftCtrl`, `RightCtrl`, etc.) when you need to target a specific side.

**Extra modifiers don't prevent matches.** A rule for `Ctrl+H` will also match when `Ctrl+Shift+H` is pressed. Use more specific triggers if you need to distinguish.

### Key names

All key names are case-sensitive and use TitleCase. Use `keymapper keys list` to print all recognized names.

- **Modifiers:** `LeftControl`, `RightControl`, `LeftCtrl`, `RightCtrl`, `LeftShift`, `RightShift`, `LeftAlt`, `RightAlt`, `LeftOption`, `RightOption`, `LeftCommand`, `RightCommand`, `LeftCmd`, `RightCmd`, `CapsLock`
- **Navigation:** `Tab`, `Space`, `Return`, `Backspace`, `Delete`, `Escape`, `UpArrow`, `DownArrow`, `LeftArrow`, `RightArrow`, `PageUp`, `PageDown`, `Home`, `End`
- **Function keys:** `F1` through `F12`
- **Letters:** `A` through `Z`
- **Numbers:** `0` through `9` (also `Number0` through `Number9`)
- **Numpad:** `Numpad0`–`Numpad9`, `NumpadDecimal`, `NumpadMultiply`, `NumpadPlus`, `NumpadClear`, `NumpadDivide`, `NumpadEnter`, `NumpadMinus`, `NumpadEqual`
- **Symbols:** `Minus`, `Equal`, `BracketLeft`, `BracketRight`, `Backslash`, `Semicolon`, `Quote`, `Comma`, `Period`, `Slash`, `Grave`, `IsoExtra`

### Common aliases

The following aliases resolve to the same platform key:

| Alias | Resolves to |
|-------|-------------|
| `Ctrl`, `LeftCtrl` | left Control key |
| `RightCtrl` | right Control key |
| `Shift`, `LeftShift` | left Shift key |
| `Alt`, `LeftAlt`, `Option`, `LeftOption` | left Alt/Option key |
| `RightAlt`, `RightOption` | right Alt/Option key |
| `Cmd`, `Command`, `Super`, `LeftCmd` | left Command/Super key |
| `RightCmd`, `RightCommand` | right Command/Super key |
| `Caps` | CapsLock |
| `Enter` | Return |
| `Esc` | Escape |
| `Up`, `Down`, `Left`, `Right` | arrow keys |
| `PgUp`, `PgDn` | PageUp, PageDown |
| `KP_Multiply`, `KP_Add`, `KP_Divide`, `KP_Enter`, `KP_Subtract` | numpad operator keys |
| `NonUSBackslash` | IsoExtra key (international keyboards) |

## CLI reference

### `keymapper appnames`

List every visible application along with the exact name keymapperd uses for matching. Use these values in the `apps` field of your config.

```
Arc
iTerm2
Keyboard Maestro Engine
Activity Monitor
```

The match is case-sensitive. On Wayland, this command prints compositor-specific alternatives (e.g. `hyprctl`, `swaymsg`) since there is no universal window-enumeration API.

### `keymapper config`

Manage the configuration file.

| Subcommand | Description |
|------------|-------------|
| `list` | Print the configuration file to stdout |
| `check [path]` | Validate and diagnose the configuration. Detects no-op rules, duplicate triggers, empty groups, and circular pairs. Accepts an optional path to a config file or directory containing `config.yaml` |
| `create [dir]` | Create an empty configuration file at the given directory or the default platform-specific location |
| `add TRIGGER OUTPUT` | Add a key-mapping rule. Options: `-g/--group NAME` (default: `"default"`), `-a/--apps APP1,APP2` (comma-separated app names) |

### `keymapper keys`

Key introspection tools.

| Subcommand | Description |
|------------|-------------|
| `list` | Print all key names recognised in the configuration file |
| `probe` | Wait for physical key presses and print each key's name and code. Press Control+Escape to exit |

### `keymapper server`

Daemon process management.

| Subcommand | Description |
|------------|-------------|
| `status` | Check whether keymapperd is running |
| `start` | Start keymapperd if it is not already running |

## Hot-reload

Edit and save your `config.yaml` while the daemon is running. Changes take effect immediately without restarting. Invalid configurations are rejected and the previous configuration is retained.

## Troubleshooting

**macOS — "Failed to create CGEventTap":** grant Accessibility permission in System Settings > Privacy & Security > Accessibility. Restart the daemon after granting access.

**Linux — "no keyboard device found":** you may need to add your user to the `input` group (`sudo usermod -aG input $USER`) and relogin.

**Rules don't take effect:** check that the `apps` value matches the actual application name. Run `keymapper appnames` to find the correct value. Omit `apps` for global rules.

**Config file not found:** the daemon searches CWD first, then the platform-specific application config directory. Use `keymapper config create` to generate a default configuration. Note that symbolic links are not followed.

## How it works

| Platform | Mechanism |
|----------|-----------|
| Linux | `evdev` device grab + `uinput` virtual keyboard |
| macOS | `CGEventTap` (requires Accessibility permission) |
| Windows | Low-level keyboard hook (`WH_KEYBOARD_LL`) |
