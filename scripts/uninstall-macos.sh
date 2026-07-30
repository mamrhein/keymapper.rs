#!/bin/bash
# ---------------------------------------------------------------------------
# Uninstalls the keymapperd launchd service from macOS.
#
# Boots out the service and removes the plist from ~/Library/LaunchAgents/.
# Does not delete log files.
# ---------------------------------------------------------------------------

set -euo pipefail

LABEL="de.adrhinum.keymapperd"
PLIST_PATH="$HOME/Library/LaunchAgents/${LABEL}.plist"

# Unload the service if it is loaded.
if launchctl print gui/"$UID" "$LABEL" >/dev/null 2>&1; then
    launchctl bootout gui/"$UID" "$LABEL"
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
