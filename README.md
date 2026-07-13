# keymapperd

Cross-platform key-remapping daemon for macOS, Linux, and Windows. Intercepts keyboard events and remaps them based on a YAML configuration file, with per-application scoping, chord (modifier + key) triggers, and hot-reload.

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
    capslock: left_control
    left_control: capslock

# Vim-style navigation in iTerm2
- name: "iterm nav"
  apps: [iTerm2]
  mappings:
    ctrl+h: left
    ctrl+j: down
    ctrl+k: up
    ctrl+l: right

# Global chord shortcuts
- name: "workspace switch"
  mappings:
    ctrl+shift+left: [cmd, left]
    ctrl+shift+right: [cmd, right]
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
| Single chord string | Replace the trigger with one key or chord | `capslock: left_control` |
| List of chord strings | Emit a sequence of keys (macro) | `f1: [cmd, t]` |

### Triggers

Triggers use compact `+`-separated strings. The last token is the base key; all preceding tokens are modifiers.

| Syntax | Example | Meaning |
|--------|---------|---------|
| Bare key | `capslock` | Single key with no modifier requirement |
| Modifier + key | `ctrl+h` | Ctrl held while pressing H |
| Multiple modifiers | `cmd+shift+t` | Cmd + Shift held while pressing T |

**Modifier matching:** when you write `ctrl`, the rule matches either left or right Control. The same applies to `shift`, `alt`, and `cmd` (which also accepts `super` and `win` as aliases). To be specific about a side, use `leftctrl`, `rightshift`, etc.

**Extra modifiers don't prevent matches.** A rule for `ctrl+h` will also match when `ctrl+shift+h` is pressed. Use more specific triggers if you need to distinguish.

### Key names

All key names are case-insensitive. Underscores are ignored, so `left_control` and `leftcontrol` are equivalent. Recognised keys include:

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
| `up`, `down`, `left`, `right` | arrow keys |
| `pgup`, `pgdn` | `pageup`, `pagedown` |

## Hot-reload

Edit and save your `config.yaml` while the daemon is running. Changes take effect immediately without restarting. Invalid configurations are rejected and the previous configuration is retained.

## Finding application names

The `apps` field matches against the running process name or bundle ID. Use these platform-specific tools to find the correct name:

| Platform | Command |
|----------|---------|
| macOS | `ls /Applications` for app bundles (use the bundle name, e.g. `Code` for VS Code), or `ps aux` for processes |
| Linux | `ps -eo comm` or `pgrep -a <name>` |
| Windows | Check the process name in Task Manager (Details tab) or use `Get-Process` in PowerShell |

The match is case-insensitive.

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
