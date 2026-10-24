#!/bin/bash
# ---------------------------------------------------------------------------
# Uninstalls the keymapperd systemd user service from Linux.
#
# Disables and stops the service, then removes the unit file. On top of that,
# removes the GNOME Shell extension installed by scripts/install-linux.sh.
# ---------------------------------------------------------------------------

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

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

# Remove the GNOME Shell extension. Deliberately not guarded on the current
# desktop session: leftovers should go away no matter where this runs, and
# the script is idempotent and quiet when the extension is absent.
if [ -f "$SCRIPT_DIR/uninstall-gnome-ext.sh" ]; then
    if ! "$SCRIPT_DIR/uninstall-gnome-ext.sh"; then
        echo "Warning: GNOME extension removal failed." >&2
        echo "Remove it manually with:" >&2
        echo "  gnome-extensions uninstall local.keymapper.active-window" >&2
    fi
else
    echo "Note: uninstall-gnome-ext.sh not found near the script." >&2
    echo "If the GNOME extension was installed, remove it with:" >&2
    echo "  gnome-extensions uninstall local.keymapper.active-window" >&2
fi
