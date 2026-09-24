#!/bin/bash
# ---------------------------------------------------------------------------
# Installs the keymapper services on macOS.
#
# Two processes are installed:
#
#   virtkbdd   — root LaunchDaemon (system domain).  Owns the Karabiner
#                DriverKit virtual-HID socket and emits mapped keys.  The
#                binary is installed to /Library/Application Support/keymapper/
#                (a root-only directory; never /usr/local/bin, which is
#                admin-writable on Intel Macs) and the plist to
#                /Library/LaunchDaemons/.
#
#   keymapperd — user LaunchAgent (gui/<UID> domain).  Captures keyboard
#                events with a CGEventTap and decides which keys are mapped.
#                The binary is installed to ~/.local/bin/keymapperd and the
#                plist to ~/Library/LaunchAgents/ of the console user.
#
# This script requires sudo privileges (for the system-domain part).  It also
# installs the Karabiner DriverKit VirtualHIDDevice package (the driver
# through which virtkbdd emits mapped keys) via install-karabiner-macos.sh.
#
# Idempotent — safe to run multiple times.
#
# Usage: scripts/install-macos.sh [keymapperd_path] [virtkbdd_path] [karabiner_pkg_path]
#   keymapperd_path      — path to the keymapperd binary (default: found via `which`).
#   virtkbdd_path        — path to the virtkbdd binary (default: found via `which`).
#   karabiner_pkg_path   — path to the Karabiner .pkg (default: bundled next to
#                          the script, or the pinned release downloaded from
#                          GitHub).
# ---------------------------------------------------------------------------

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

KEYMAPPERD_LABEL="de.adrhinum.keymapperd"
VIRTKBDD_LABEL="de.adrhinum.virtkbdd"

# Canonical install locations.  The root daemon binary deliberately does not
# live in /usr/local/bin: that directory (and /usr/local itself) is admin-
# writable on Intel Macs — and stays admin-writable under Homebrew on Apple
# Silicon — so a daemon binary there could be replaced by any process of an
# admin user and would then be run as root by launchd.
VIRTKBDD_BIN_DIR="/Library/Application Support/keymapper"
VIRTKBDD_BIN="${VIRTKBDD_BIN_DIR}/virtkbdd"
# Legacy location of the daemon binary (removed on upgrade).
LEGACY_VIRTKBDD_BIN="/usr/local/bin/virtkbdd"
LAUNCH_DAEMONS_DIR="/Library/LaunchDaemons"
VIRTKBDD_LOG_DIR="/var/log/virtkbdd"

# Find a plist template.  It may be alongside the script (DMG layout) or
# under ../resources/launchd/ (repo layout).
find_template() {
    local label="$1"
    if [ -f "$SCRIPT_DIR/resources/launchd/${label}.plist" ]; then
        echo "$SCRIPT_DIR/resources/launchd/${label}.plist"
    elif [ -f "$SCRIPT_DIR/../resources/launchd/${label}.plist" ]; then
        echo "$(cd "$SCRIPT_DIR/.." && pwd)/resources/launchd/${label}.plist"
    else
        echo "Error: launchd plist template for ${label} not found near the script." >&2
        exit 1
    fi
}

KEYMAPPERD_TEMPLATE="$(find_template "$KEYMAPPERD_LABEL")"
VIRTKBDD_TEMPLATE="$(find_template "$VIRTKBDD_LABEL")"

# Resolve the binary source paths.
if [ $# -ge 1 ]; then
    KEYMAPPERD_SRC="$1"
else
    KEYMAPPERD_SRC="$(which keymapperd 2>/dev/null || true)"
fi

if [ $# -ge 2 ]; then
    VIRTKBDD_SRC="$2"
else
    VIRTKBDD_SRC="$(which virtkbdd 2>/dev/null || true)"
fi

for src in "$KEYMAPPERD_SRC" "$VIRTKBDD_SRC"; do
    if [ -z "$src" ] || [ ! -x "$src" ]; then
        echo "Error: binary not found or not executable: '${src:-<missing>}'." >&2
        echo "Provide the keymapperd and virtkbdd paths as arguments, or ensure both are in \$PATH." >&2
        exit 1
    fi
done

# Require root — the virtkbdd LaunchDaemon is system-wide.
if [ "$EUID" -ne 0 ]; then
    echo "This script must be run as root (use sudo)." >&2
    exit 1
fi

# Resolve the console user (owner of /dev/console).  keymapperd runs in that
# user's gui domain.
CONSOLE_USER="$(stat -f '%Su' /dev/console)"
CONSOLE_UID="$(id -u "$CONSOLE_USER")"
CONSOLE_HOME="$(dscl . -read "/Users/${CONSOLE_USER}" NFSHomeDirectory | awk '{print $2}')"

if [ -z "$CONSOLE_HOME" ] || [ ! -d "$CONSOLE_HOME" ]; then
    echo "Error: could not resolve the home directory of console user '${CONSOLE_USER}'." >&2
    exit 1
fi

# Run a launchctl command in the console user's gui domain.  This script runs
# as root (brew invokes it with sudo), and a root process cannot reach another
# user's gui/<UID> domain directly — `launchctl bootstrap gui/<UID>` fails with
# an input/output error.  `launchctl asuser` establishes the proper bootstrap
# port for that user's domain, so the gui-domain verbs below succeed.
gui_launchctl() {
    launchctl asuser "$CONSOLE_UID" launchctl "$@"
}

KEYMAPPERD_BIN="${CONSOLE_HOME}/.local/bin/keymapperd"
LAUNCH_AGENTS_DIR="${CONSOLE_HOME}/Library/LaunchAgents"
KEYMAPPERD_LOG_DIR="${CONSOLE_HOME}/Library/Logs/keymapper"

# Remove the com.apple.quarantine xattr from a file, if present.  brew leaves
# this on the files extracted from the release archive, and `cp`/`install`
# carry it onto the installed copies.  launchd refuses to trust a quarantined
# service definition (error 155), so every file we install must be clean of it.
dequarantine() {
    xattr -d com.apple.quarantine "$1" 2>/dev/null || true
}

# Assert that the given directory and every one of its parent directories are
# writable only by their owner (no group or world write bit anywhere in the
# chain).  A binary that launchd runs as root must not live below a directory
# that non-root users can write into: any such user could delete and replace
# the binary, which means arbitrary code execution as root at the next load.
# Fails closed — the install aborts instead of writing into an insecure path.
assert_secure_path() {
    local path="$1"
    local mode
    while true; do
        mode="$(stat -f '%OLp' "$path")"
        # The two lowest octal digits are the group and world permission
        # bits; the write bit (2) in either lets non-owner users replace the
        # directory's contents.  A symlinked component reports mode 777 and
        # is rejected as well.
        if [ $(( (8#$mode / 8) % 8 & 2 )) -ne 0 ] || [ $(( 8#$mode % 8 & 2 )) -ne 0 ]; then
            echo "Error: refusing to install into '${1}': '${path}' is group-" >&2
            echo "or world-writable (mode ${mode}).  A root-run daemon binary must" >&2
            echo "not live below a directory that non-root users can write into."
            exit 1
        fi
        [ "$path" = "/" ] && break
        path="$(dirname "$path")"
    done
}

# Copy a binary to its canonical location (skipping the copy when source and
# destination are the same file), then fix ownership.
install_binary() {
    local src="$1" dst="$2" owner="$3"
    mkdir -p "$(dirname "$dst")"
    if [ ! "$src" -ef "$dst" ]; then
        install -m 755 "$src" "$dst"
    fi
    dequarantine "$dst"
    chown "$owner" "$dst"
}

# ---------------------------------------------------------------------------
# virtkbdd — root LaunchDaemon (system domain)
# ---------------------------------------------------------------------------

echo "Installing virtkbdd (LaunchDaemon)..."

# Remove a virtkbdd binary left over from older releases (the daemon used to
# live in admin-writable /usr/local/bin — see the note on the canonical
# install locations above).  A dangling root-executable copy there would
# survive this script, so drop it once the daemon has a secure home.
if [ -f "$LEGACY_VIRTKBDD_BIN" ]; then
    rm "$LEGACY_VIRTKBDD_BIN"
    echo "Removed the legacy ${LEGACY_VIRTKBDD_BIN}."
fi

mkdir -p "$VIRTKBDD_BIN_DIR"
mkdir -p "$LAUNCH_DAEMONS_DIR"
mkdir -p "$VIRTKBDD_LOG_DIR"

# Install the root daemon and its plist only into root-only-writable
# directory chains; abort otherwise (SEC-04).
assert_secure_path "$VIRTKBDD_BIN_DIR"
assert_secure_path "$LAUNCH_DAEMONS_DIR"

install_binary "$VIRTKBDD_SRC" "$VIRTKBDD_BIN" root:wheel

# If the service is already loaded, unload it first so we can replace the plist.
if launchctl print system/"$VIRTKBDD_LABEL" >/dev/null 2>&1; then
    launchctl bootout system/"$VIRTKBDD_LABEL" 2>/dev/null || true
fi

sed \
    -e "s|@BINARY_PATH@|$VIRTKBDD_BIN|g" \
    -e "s|@LOG_DIR@|$VIRTKBDD_LOG_DIR|g" \
    "$VIRTKBDD_TEMPLATE" > "$LAUNCH_DAEMONS_DIR/${VIRTKBDD_LABEL}.plist"
dequarantine "$LAUNCH_DAEMONS_DIR/${VIRTKBDD_LABEL}.plist"
chown root:wheel "$LAUNCH_DAEMONS_DIR/${VIRTKBDD_LABEL}.plist"
chmod 644 "$LAUNCH_DAEMONS_DIR/${VIRTKBDD_LABEL}.plist"

echo "Installed ${VIRTKBDD_LABEL}.plist to ${LAUNCH_DAEMONS_DIR}/"

launchctl bootstrap system "$LAUNCH_DAEMONS_DIR/${VIRTKBDD_LABEL}.plist"

if launchctl print system/"$VIRTKBDD_LABEL" >/dev/null 2>&1; then
    echo "virtkbdd is running via launchd."
else
    echo "Warning: virtkbdd was installed but does not appear to be running." >&2
    echo "Check logs at ${VIRTKBDD_LOG_DIR}/virtkbdd-err.log" >&2
fi

# ---------------------------------------------------------------------------
# keymapperd — user LaunchAgent (gui/<UID> domain)
# ---------------------------------------------------------------------------

echo ""
echo "Installing keymapperd (LaunchAgent for ${CONSOLE_USER})..."

# Remove a keymapperd LaunchDaemon left over from older releases (the daemon
# moved to the user domain).  Without this, both the legacy root process and
# the new user process would capture and remap keys.
if launchctl print system/"$KEYMAPPERD_LABEL" >/dev/null 2>&1; then
    launchctl bootout system/"$KEYMAPPERD_LABEL" 2>/dev/null || true
    echo "Stopped the legacy keymapperd LaunchDaemon."
fi
if [ -f "$LAUNCH_DAEMONS_DIR/${KEYMAPPERD_LABEL}.plist" ]; then
    rm "$LAUNCH_DAEMONS_DIR/${KEYMAPPERD_LABEL}.plist"
    echo "Removed the legacy ${LAUNCH_DAEMONS_DIR}/${KEYMAPPERD_LABEL}.plist"
fi

install_binary "$KEYMAPPERD_SRC" "$KEYMAPPERD_BIN" "$CONSOLE_USER"
mkdir -p "$LAUNCH_AGENTS_DIR"
mkdir -p "$KEYMAPPERD_LOG_DIR"
chown "$CONSOLE_USER" "$KEYMAPPERD_LOG_DIR"

# If the service is already loaded, unload it first so we can replace the plist.
# All gui-domain verbs go through `gui_launchctl` (see above) so they run in the
# console user's domain rather than root's.
if gui_launchctl print "gui/${CONSOLE_UID}/${KEYMAPPERD_LABEL}" >/dev/null 2>&1; then
    gui_launchctl bootout "gui/${CONSOLE_UID}/${KEYMAPPERD_LABEL}" 2>/dev/null || true
fi

sed \
    -e "s|@BINARY_PATH@|$KEYMAPPERD_BIN|g" \
    "$KEYMAPPERD_TEMPLATE" > "${LAUNCH_AGENTS_DIR}/${KEYMAPPERD_LABEL}.plist"
dequarantine "${LAUNCH_AGENTS_DIR}/${KEYMAPPERD_LABEL}.plist"
chown "$CONSOLE_USER" "${LAUNCH_AGENTS_DIR}/${KEYMAPPERD_LABEL}.plist"
chmod 644 "${LAUNCH_AGENTS_DIR}/${KEYMAPPERD_LABEL}.plist"

echo "Installed ${KEYMAPPERD_LABEL}.plist to ${LAUNCH_AGENTS_DIR}/"

gui_launchctl bootstrap "gui/${CONSOLE_UID}" "${LAUNCH_AGENTS_DIR}/${KEYMAPPERD_LABEL}.plist"

if gui_launchctl print "gui/${CONSOLE_UID}/${KEYMAPPERD_LABEL}" >/dev/null 2>&1; then
    echo "keymapperd is running via launchd."
else
    echo "Warning: keymapperd was installed but does not appear to be running." >&2
    echo "Check the log file: ${KEYMAPPERD_LOG_DIR}/keymapperd.log" >&2
fi

# ---------------------------------------------------------------------------
# Karabiner DriverKit package
# ---------------------------------------------------------------------------

# Install the Karabiner DriverKit package (pkg install, driver activation,
# and the daemon LaunchDaemon).  An explicit pkg path is passed through when
# given (the DMG bundles one).
echo ""
if [ $# -ge 3 ]; then
    "${SCRIPT_DIR}/install-karabiner-macos.sh" "$3"
else
    "${SCRIPT_DIR}/install-karabiner-macos.sh"
fi

# ---------------------------------------------------------------------------
# TCC instructions
# ---------------------------------------------------------------------------

echo ""
echo "Grant keymapperd the required privacy permissions:"
echo "  System Settings > Privacy & Security > Input Monitoring"
echo "      enable keymapperd (required to see keyboard events)"
echo "  System Settings > Privacy & Security > Accessibility"
echo "      enable keymapperd (required to swallow mapped keys)"
echo ""
echo "Then restart the services:  keymapper daemon restart"
