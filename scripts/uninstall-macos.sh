#!/bin/bash
# ---------------------------------------------------------------------------
# Uninstalls the keymapperd LaunchDaemon from macOS.
#
# Boots out the service and removes the plist from /Library/LaunchDaemons/.
# Does not delete log files.  Requires sudo privileges.
# ---------------------------------------------------------------------------

set -euo pipefail

LABEL="de.adrhinum.keymapperd"
PLIST_PATH="/Library/LaunchDaemons/${LABEL}.plist"

# Require root.
if [ "$EUID" -ne 0 ]; then
    echo "This script must be run as root (use sudo)." >&2
    exit 1
fi

# Unload the service if it is loaded.
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
