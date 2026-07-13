# keymapperd

Cross-platform key-remapping daemon for macOS, Linux, and Windows. Intercepts keyboard events and remaps them based on a YAML configuration file, with per-application scoping, chord (modifier + key) triggers and outputs, and hot-reload.

## Installation

Requires Rust 1.85+ (edition 2024).

```bash
cargo install --path .
```

Run with appropriate privileges for keyboard interception (Accessibility on macOS, `/dev/input` access on Linux).

## Configuration

Create `config.yaml` in one of the following locations:

| Platform | Path |
|----------|------|
| Linux | `$XDG_CONFIG_HOME/keymapperd/config.yaml` (defaults to `~/.config/keymapperd/`) |
| macOS | `~/Library/Application Support/keymapperd/config.yaml` |
| Windows | `%APPDATA%\keymapperd\config.yaml` |

The daemon exits if no configuration file is found.

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

# Modifier remapping — emit LeftAlt+L when pressing OptionRight
- mappings:
    OptionRight: LeftAlt+L
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
| List of chord strings | Emit a sequence of key events (macro) | `F1: [Cmd, T]` |

Every output is a **chord**: modifier keys are held while the base key is pressed, then released in reverse. For example, `Cmd+Left` is emitted as "press Cmd → press Left → release Left → release Cmd", ensuring the modifier has its intended effect.

### Triggers

Triggers use compact `+`-separated strings. The last token is the base key; all preceding tokens are modifiers.

| Syntax | Example | Meaning |
|--------|---------|---------|
| Bare key | `CapsLock` | Single key with no modifier requirement |
| Modifier + key | `Ctrl+H` | Ctrl held while pressing H |
| Multiple modifiers | `Cmd+Shift+T` | Cmd + Shift held while pressing T |

**Modifier matching:** when you write `Ctrl`, the rule matches either left or right Control. The same applies to `Shift`, `Alt`, and `Cmd` (which also accepts `Super` and `Win` as aliases).

**Extra modifiers don't prevent matches.** A rule for `Ctrl+H` will also match when `Ctrl+Shift+H` is pressed. Use more specific triggers if you need to distinguish.

### Key names

All key names are case-sensitive and use PascalCase. Recognised keys include:

- **Modifiers:** `LeftControl`, `RightControl`, `LeftCtrl`, `RightCtrl`, `LeftShift`, `RightShift`, `LeftAlt`, `RightAlt`, `OptionLeft`, `OptionRight`, `LeftCommand`, `RightCommand`, `CapsLock`
- **Navigation:** `Tab`, `Space`, `Return`, `Backspace`, `Delete`, `Escape`, `UpArrow`, `DownArrow`, `LeftArrow`, `RightArrow`, `PageUp`, `PageDown`, `Home`, `End`
- **Function keys:** `F1` through `F12`
- **Letters:** `A` through `Z`
- **Numbers:** `0` through `9`

### Common aliases

The following aliases resolve to the same platform key:

| Alias | Resolves to |
|-------|-------------|
| `Ctrl`, `LeftCtrl` | left Control key |
| `RightCtrl` | right Control key |
| `Shift`, `LeftShift` | left Shift key |
| `Alt`, `LeftAlt`, `Option`, `OptionLeft` | left Alt/Option key |
| `RightAlt`, `OptionRight` | right Alt/Option key |
| `Cmd`, `Command`, `Super` | left Command/Super key |
| `RightCmd`, `RightCommand` | right Command/Super key |
| `Caps` | CapsLock |
| `Enter` | Return |
| `Esc` | Escape |
| `Up`, `Down`, `Left`, `Right` | arrow keys |
| `PgUp`, `PgDn` | PageUp, PageDown |

## Hot-reload

Edit and save your `config.yaml` while the daemon is running. Changes take effect immediately without restarting. Invalid configurations are rejected and the previous configuration is retained.

## Finding application names

The `apps` field matches against the running process name or bundle ID. Use these platform-specific tools to find the correct name:

| Platform | Command |
|----------|---------|
| macOS | `ls /Applications` for app bundles (use the bundle name, e.g. `Code` for VS Code), or `ps aux` for processes |
| Linux | `ps -eo comm` or `pgrep -a <name>` |
| Windows | Check the process name in Task Manager (Details tab) or use `Get-Process` in PowerShell |

The match is case-sensitive.

## Troubleshooting

**macOS — "Failed to create CGEventTap":** grant Accessibility permission in System Settings > Privacy & Security > Accessibility. Restart the daemon after granting access.

**Linux — "no keyboard device found":** you may need to add your user to the `input` group (`sudo usermod -aG input $USER`) and relogin.

**Rules don't take effect:** check that the `apps` value matches the actual process name. Use the commands in the section above to find it. Omit `apps` for global rules.

## How it works

| Platform | Mechanism |
|----------|-----------|
| Linux | `evdev` device grab + `uinput` virtual keyboard |
| macOS | `CGEventTap` (requires Accessibility permission) |
| Windows | Low-level keyboard hook (`WH_KEYBOARD_LL`) |
