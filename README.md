# keymapper

Cross-platform key-remapping daemon and CLI utility for macOS, Linux, and Windows. Intercepts keyboard events and remaps them based on a YAML configuration file, with per-application scoping, chord (modifier + key) triggers and outputs, hot-reload, and macros.

The project ships two binaries:

- **`keymapperd`** — the background daemon that intercepts keyboard events and applies remapping rules.
- **`keymapper`** — a CLI utility for managing configuration, inspecting keys, and controlling the daemon.

## Installation

Building from source requires Rust 1.95+ (edition 2024).

### macOS

The daemon must run as root (required for IOKit device seizure), and remapped keys are emitted through the Karabiner DriverKit VirtualHIDDevice driver.

**Homebrew:**

```bash
brew install keymapper
```

This builds the Rust binaries from source and installs the Karabiner DriverKit VirtualHIDDevice driver (via a cask dependency). The driver setup requires sudo.

The driver must be enabled in System Settings > General > Login Items & Extensions > Driver Extensions on first run. No reboot is required. See [macos-architecture.md](docs/macos-architecture.md) for details.

Start the service:

```bash
brew services start keymapper
```

**Pre-built DMG:**

Download a pre-built DMG from the [releases page](https://github.com/mamrhein/keymapper.rs/releases), mount it, and run:

```bash
sudo ./install.sh
```

This installs the binaries to `/usr/local/bin`, registers the LaunchDaemon, and installs the Karabiner DriverKit driver.

**From source:**

```bash
cargo install --path .
sudo scripts/install-macos.sh /usr/local/bin/keymapperd
```

The script registers the LaunchDaemon and installs the Karabiner DriverKit driver.

### Linux

**Pre-built archive:**

Download the pre-built tar.xz for your architecture from the [releases page](https://github.com/mamrhein/keymapper.rs/releases), extract it, and copy the binaries to a directory on your `PATH`:

```bash
tar -xJf keymapper-vX.Y.Z-x86_64-unknown-linux-gnu.tar.xz
cd keymapper/vX.Y.Z
install -m 755 keymapper keymapperd ~/.local/bin/
./install-linux.sh ~/.local/bin/keymapperd
```

**From source:**

```bash
cargo install --path .
scripts/install-linux.sh
```

The script installs the systemd user service, enables it at login, and starts keymapperd. The daemon needs read access to `/dev/input/event*` (usually via the `input` group) and write access to `/dev/uinput`; if it reports "no keyboard device found", see [Troubleshooting](#troubleshooting).

### Windows

```bash
cargo install --path .
```

Run `keymapperd` directly; there is no service-manager integration on Windows. Input capture uses a low-level keyboard hook, and event emission uses `SendInput` (see [windows-architecture.md](docs/windows-architecture.md)).

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
```

## Configuration

Create `config.yaml` in one of the following locations:

| Platform | Path                                                                            |
| -------- | ------------------------------------------------------------------------------- |
| Linux    | `$XDG_CONFIG_HOME/keymapperd/config.yaml` (defaults to `~/.config/keymapperd/`) |
| macOS    | `~/Library/Application Support/keymapperd/config.yaml`                          |
| Windows  | `%APPDATA%\keymapperd\config.yaml`                                              |

The daemon searches the platform-specific application config directory. Symbolic links are rejected; `config.yaml` must be a regular file.

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
````

### Structure

The document is a YAML sequence of rule groups. Each group has:

| Field       | Required | Description                                                                        |
| ----------- | -------- | ---------------------------------------------------------------------------------- |
| `name`      | No       | Human-readable label (ignored at runtime)                                          |
| `apps`      | No       | List of application names to scope the group. Omit or leave empty for global rules |
| `keyboards` | No       | List of keyboard filters to scope the group. Omit or leave empty for all keyboards |
| `mappings`  | Yes      | Key-value pairs mapping triggers to outputs                                        |

Groups are evaluated in definition order. Within each group, mappings are evaluated top-to-bottom; the first matching trigger wins.

### Keyboard filters

Groups can be scoped to specific keyboards with the `keyboards` field. Each filter is a mapping of one or more of `name`, `vendor`, `model`, and `port`; a keyboard matches when all provided fields match (case-insensitive), and multiple filters form an OR set. Omit `keyboards` or leave it empty to apply to all keyboards.

```yaml
# Only apply this group when the event comes from an Apple Magic Keyboard
- name: "magic keyboard only"
  keyboards:
    - name: Magic Keyboard
      vendor: Apple
  mappings:
    CapsLock: LeftControl
```

A document-level `keyboards` filter (in the mapping form, alongside `groups`) restricts which keyboards are processed at all. Use `keymapper keyboards` to list the available values.

### Mappings

Each mapping is a `trigger: output` pair inside a `mappings:` block.

| Output                     | Description                                                                 | Example                 |
| -------------------------- | --------------------------------------------------------------------------- | ----------------------- |
| Single key or chord string | Replace the trigger with one key event (modifiers held while pressing base) | `CapsLock: LeftControl` |
| List of chord strings      | Emit a sequence of key events (macro)                                       | `F1: [Cmd+C, T]`        |

Every output is a **chord**: modifier keys are held while the base key is pressed, then released in reverse. For example, `Cmd+C` is emitted as "press Cmd → press C → release C → release Cmd", ensuring the modifier has its intended effect.

### Triggers

Triggers use compact `+`-separated strings. The last token is the base key; all preceding tokens are modifiers.

| Syntax             | Example       | Meaning                                 |
| ------------------ | ------------- | --------------------------------------- |
| Bare key           | `CapsLock`    | Single key with no modifier requirement |
| Modifier + key     | `Ctrl+H`      | Ctrl held while pressing H              |
| Multiple modifiers | `Cmd+Shift+T` | Cmd + Shift held while pressing T       |

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
- **Symbols:** `Minus`, `Equal`, `BracketLeft`, `BracketRight`, `Backslash`, `Semicolon`, `Quote`, `Comma`, `Period`, `Slash`, `Grave`, `IsoExtra`, `IsoHash`
- **Media:** `PlayPause`, `VolumeUp`, `VolumeDown`, `Mute`, `NextTrack`, `PreviousTrack`, `Stop`
- **Display:** `BrightnessUp`, `BrightnessDown`

### Common aliases

The following aliases resolve to the same platform key:

| Alias                                                           | Resolves to                            |
| --------------------------------------------------------------- | -------------------------------------- |
| `Ctrl`, `LeftCtrl`                                              | left Control key                       |
| `RightCtrl`                                                     | right Control key                      |
| `Shift`, `LeftShift`                                            | left Shift key                         |
| `Alt`, `LeftAlt`, `Option`, `LeftOption`                        | left Alt/Option key                    |
| `RightAlt`, `RightOption`                                       | right Alt/Option key                   |
| `Cmd`, `Command`, `Super`, `LeftCmd`                            | left Command/Super key                 |
| `RightCmd`, `RightCommand`                                      | right Command/Super key                |
| `Caps`                                                          | CapsLock                               |
| `Enter`                                                         | Return                                 |
| `Esc`                                                           | Escape                                 |
| `Up`, `Down`, `Left`, `Right`                                   | arrow keys                             |
| `PgUp`, `PgDn`                                                  | PageUp, PageDown                       |
| `KP_Multiply`, `KP_Add`, `KP_Divide`, `KP_Enter`, `KP_Subtract` | numpad operator keys                   |
| `NonUSBackslash`                                                | IsoExtra key (international keyboards) |
| `Hash`                                                          | IsoHash key (international keyboards)  |
| `Play`                                                          | PlayPause                              |
| `VolUp`, `VolDown`, `VolMute`                                   | VolumeUp, VolumeDown, Mute             |
| `ScanNext`, `ScanPrev`, `MediaStop`                             | NextTrack, PreviousTrack, Stop         |

## CLI reference

### `keymapper appnames`

List every visible application along with the exact name keymapperd uses for matching. Use these values in the `apps` field of your config.

```
Arc
iTerm2
Keyboard Maestro Engine
Activity Monitor
```

The match is case-sensitive. On Linux, the names are `.desktop` application ids (e.g. `org.mozilla.firefox`), resolved uniformly across X11 and Wayland.

### `keymapper config`

Manage the configuration file.

| Subcommand           | Description                                                                                                                                                                                         |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `list`               | Print the configuration file to stdout                                                                                                                                                              |
| `check [path]`       | Validate and diagnose the configuration. Detects no-op rules, duplicate triggers, empty groups, and circular pairs. Accepts an optional path to a config file or directory containing `config.yaml` |
| `create [dir]`       | Create an empty configuration file at the given directory or the default platform-specific location                                                                                                 |
| `add TRIGGER OUTPUT` | Add a key-mapping rule. Options: `-g/--group NAME` (default: `"default"`), `-a/--apps APP1,APP2` (comma-separated app names), `--keyboard SPEC` and `--keyboards-global SPEC` (keyboard filters, key=value pairs)                                                                        |

### `keymapper keyboards`

List all connected keyboard devices, printing each keyboard's name, vendor, model, port type, and device identifier. The name, vendor, model, and port values can be used in the `--keyboard` / `--keyboards-global` filters (as key=value pairs) to scope rules to specific devices.

### `keymapper keys`

Key introspection tools.

| Subcommand | Description                                                                                    |
| ---------- | ---------------------------------------------------------------------------------------------- |
| `list`     | Print all key names recognised in the configuration file                                       |
| `probe`    | Wait for physical key presses and print each key's name and code. Press Control+Escape to exit |

### `keymapper daemon`

Daemon process management.

All subcommands accept an optional `--config-dir DIR` flag that selects the process-management backend, chosen once per invocation so a `start` and a later `stop` always target the same mechanism:

- **Omitted** (production mode) — manages keymapperd through the platform service manager: `launchctl` on macOS, `systemctl --user` on Linux, or a direct spawn on Windows.
- **`--config-dir DIR`** (development mode) — spawns keymapperd as a detached background process with `DIR` as its working directory, tracked through `DIR/keymapperd.pid`.

| Subcommand | Description                                   |
| ---------- | --------------------------------------------- |
| `status`   | Check whether keymapperd is running           |
| `start`    | Start keymapperd if it is not already running |
| `stop`     | Stop keymapperd if it is running              |
| `restart`  | Restart keymapperd (stop then start)          |

## Hot-reload

Edit and save your `config.yaml` while the daemon is running. Changes take effect immediately without restarting. Invalid configurations are rejected and the previous configuration is retained.

## Troubleshooting

**macOS — daemon not capturing keys:** the daemon must run as root to seize HID devices via IOKit. Verify it is running: `launchctl print system/de.adrhinum.keymapperd`. If it is not loaded, install the LaunchDaemon: `sudo ./install-macos.sh /usr/local/bin/keymapperd`.

**macOS — driver not loaded:** check that the Karabiner DriverKit extension is enabled in System Settings > General > Login Items & Extensions > Driver Extensions. No reboot is required. See [macos-architecture.md](docs/macos-architecture.md) for full troubleshooting.

**Linux — "no keyboard device found":** you may need to add your user to the `input` group (`sudo usermod -aG input $USER`) and relogin.

**Linux — daemon exits at startup with a permission error:** creating the virtual keyboard requires write access to `/dev/uinput`. Grant it with a udev rule, ensure the user is in the `input` group, and relogin:

```bash
echo 'KERNEL=="uinput", MODE="0660", GROUP="input"' | sudo tee /etc/udev/rules.d/99-keymapper.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
sudo usermod -aG input $USER
```

**Rules don't take effect:** check that the `apps` value matches the actual application name. Run `keymapper appnames` to find the correct value. Omit `apps` for global rules.

**Config file not found:** the daemon searches the platform-specific application config directory. Use `keymapper config create` to generate a default configuration. Note that symbolic links are not followed.

## How it works

| Platform | Mechanism                                                                                                                                                                       |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Linux    | `evdev` device grab + `uinput` virtual keyboard                                                                                                                                 |
| macOS    | IOKit device seizure for input capture, Karabiner DriverKit daemon for event emission                                                                                           |
| Windows  | Low-level keyboard hook (`WH_KEYBOARD_LL`) for capture, `SendInput` for emission (see [windows-architecture.md](docs/windows-architecture.md))                                  |
