#!/bin/bash
# ---------------------------------------------------------------------------
# Uninstalls the keymapperd systemd user service from Linux.
#
# Disables and stops the service, then removes the unit file.
# ---------------------------------------------------------------------------

set -euo pipefail

UNIT_NAME="keymapperd.service"
UNIT_PATH="$HOME/.config/systemd/user/${UNIT_NAME}"

# Disable and stop the service if it is active.
if systemctl --user is-active "$UNIT_NAME" >/dev/null 2>&1 || \
   systemctl --user is-enabled "$UNIT_NAME" >/dev/null 2>&1; then
    systemctl --user disable --now "$UNIT_NAME"
    echo "Stopped and disabled ${UNIT_NAME}."
else
    echo "${UNIT_NAME} is not active."
fi

# Remove the unit file.
if [ -f "$UNIT_PATH" ]; then
    rm "$UNIT_PATH"
    echo "Removed ${UNIT_PATH}"
else
    echo "No unit found at ${UNIT_PATH}"
fi

# Reload the systemd user manager.
systemctl --user daemon-reload
