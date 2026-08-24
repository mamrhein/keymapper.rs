#!/bin/bash
# ---------------------------------------------------------------------------
# Installs the keymapperd LaunchDaemon on macOS.
#
# Copies the plist template to /Library/LaunchDaemons/, resolves the binary
# path, and boots the service.  The daemon runs as root to perform IOKit
# device seizure.  This script requires sudo privileges.
#
# It also installs the Karabiner DriverKit VirtualHIDDevice package (the
# driver through which keymapperd emits remapped keys) via
# install-karabiner-macos.sh.
#
# Idempotent — safe to run multiple times.
#
# Usage: scripts/install-macos.sh [binary_path] [karabiner_pkg_path]
#   binary_path        — absolute path to keymapperd (default: found via `which`).
#   karabiner_pkg_path — path to the Karabiner .pkg (default: bundled next to
#                        the script, or the pinned release downloaded from
#                        GitHub).
# ---------------------------------------------------------------------------

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

LABEL="de.adrhinum.keymapperd"
LAUNCH_DAEMONS_DIR="/Library/LaunchDaemons"
LOG_DIR="/var/log/keymapperd"

# Find the plist template.  It may be alongside the script (DMG layout) or
# under ../resources/launchd/ (repo layout).
if [ -f "$SCRIPT_DIR/resources/launchd/${LABEL}.plist" ]; then
    PLIST_TEMPLATE="$SCRIPT_DIR/resources/launchd/${LABEL}.plist"
elif [ -f "$SCRIPT_DIR/../resources/launchd/${LABEL}.plist" ]; then
    PLIST_TEMPLATE="$(cd "$SCRIPT_DIR/.." && pwd)/resources/launchd/${LABEL}.plist"
else
    echo "Error: launchd plist template not found near the script." >&2
    exit 1
fi

# Resolve the keymapperd binary path.
if [ $# -ge 1 ]; then
    BINARY_PATH="$1"
else
    BINARY_PATH="$(which keymapperd 2>/dev/null || true)"
fi

if [ -z "$BINARY_PATH" ]; then
    echo "Error: keymapperd binary not found. Provide its path as an argument or ensure it is in \$PATH." >&2
    exit 1
fi

if [ ! -x "$BINARY_PATH" ]; then
    echo "Error: '$BINARY_PATH' is not an executable file." >&2
    exit 1
fi

# Require root — LaunchDaemons are system-wide and managed by launchd as root.
if [ "$EUID" -ne 0 ]; then
    echo "This script must be run as root (use sudo)." >&2
    exit 1
fi

# Ensure directories exist.
mkdir -p "$LAUNCH_DAEMONS_DIR"
mkdir -p "$LOG_DIR"

# If the service is already loaded, unload it first so we can replace the plist.
if launchctl print system/"$LABEL" >/dev/null 2>&1; then
    launchctl bootout system/"$LABEL" 2>/dev/null || true
fi

# Copy the plist template and substitute placeholders.
sed \
    -e "s|@BINARY_PATH@|$BINARY_PATH|g" \
    -e "s|@LOG_DIR@|$LOG_DIR|g" \
    "$PLIST_TEMPLATE" > "$LAUNCH_DAEMONS_DIR/${LABEL}.plist"

# Set correct ownership and permissions for LaunchDaemons.
chown root:wheel "$LAUNCH_DAEMONS_DIR/${LABEL}.plist"
chmod 644 "$LAUNCH_DAEMONS_DIR/${LABEL}.plist"

echo "Installed ${LABEL}.plist to ${LAUNCH_DAEMONS_DIR}/"

# Boot the service.
launchctl bootstrap system "$LAUNCH_DAEMONS_DIR/${LABEL}.plist"

if launchctl print system/"$LABEL" >/dev/null 2>&1; then
    echo "keymapperd is running via launchd."
else
    echo "Warning: service was installed but does not appear to be running." >&2
    echo "Check logs at ${LOG_DIR}/keymapperd-err.log" >&2
fi

# Install the Karabiner DriverKit package (pkg install, driver activation,
# and the daemon LaunchDaemon).  An explicit pkg path is passed through when
# given (the DMG bundles one).
echo ""
if [ $# -ge 2 ]; then
    "${SCRIPT_DIR}/install-karabiner-macos.sh" "$2"
else
    "${SCRIPT_DIR}/install-karabiner-macos.sh"
fi
