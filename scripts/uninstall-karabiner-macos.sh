#!/bin/bash
# ---------------------------------------------------------------------------
# Uninstalls the Karabiner DriverKit VirtualHIDDevice package from macOS.
#
# Boots out the Karabiner daemon LaunchDaemon, deactivates the DriverKit
# extension, and removes the package files (via the remove_files.sh script
# shipped with the package).  Requires sudo privileges.
# ---------------------------------------------------------------------------

set -euo pipefail

KARABINER_APP_DIR="/Library/Application Support/org.pqrs/Karabiner-DriverKit-VirtualHIDDevice"
MANAGER_BIN="/Applications/.Karabiner-VirtualHIDDevice-Manager.app/Contents/MacOS/Karabiner-VirtualHIDDevice-Manager"
REMOVE_FILES_SH="${KARABINER_APP_DIR}/scripts/uninstall/remove_files.sh"

LABEL="org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon"
PLIST_PATH="/Library/LaunchDaemons/${LABEL}.plist"

# Require root.
if [ "$EUID" -ne 0 ]; then
    echo "This script must be run as root (use sudo)." >&2
    exit 1
fi

# Stop the Karabiner daemon first so it does not restart while files are
# removed (the LaunchDaemon has KeepAlive).
if launchctl print system/"$LABEL" >/dev/null 2>&1; then
    launchctl bootout system/"$LABEL"
    echo "Stopped ${LABEL}."
else
    echo "${LABEL} is not loaded."
fi

# Remove the plist.
if [ -f "$PLIST_PATH" ]; then
    rm "$PLIST_PATH"
    echo "Removed ${PLIST_PATH}"
else
    echo "No plist found at ${PLIST_PATH}"
fi

# Deactivate the DriverKit extension.
if [ -x "$MANAGER_BIN" ]; then
    echo "Deactivating the DriverKit extension..."
    if ! "$MANAGER_BIN" deactivate; then
        echo "Warning: 'deactivate' returned an error; continuing with file removal." >&2
    fi
else
    echo "Manager binary not found; skipping deactivation."
fi

# Remove the package files (manager app, Application Support directory, tmp).
if [ -f "$REMOVE_FILES_SH" ]; then
    # Copy the script out first: it removes its own directory while running,
    # which would truncate the file bash is reading from.
    TMP_REMOVE="$(mktemp)"
    cp "$REMOVE_FILES_SH" "$TMP_REMOVE"
    bash "$TMP_REMOVE"
    rm -f "$TMP_REMOVE"
else
    echo "remove_files.sh not found; removing files manually."
    rm -rf "/Applications/.Karabiner-VirtualHIDDevice-Manager.app"
    rm -rf "$KARABINER_APP_DIR"
fi

echo "Karabiner DriverKit package removed."
