#!/bin/bash
# ---------------------------------------------------------------------------
# Installs the keymapperd systemd user service on Linux.
#
# Copies the unit template to ~/.config/systemd/user/, resolves the binary
# path, enables and starts the service. On GNOME, additionally installs the
# Shell extension via scripts/install-gnome-ext.sh. Idempotent — safe to run
# multiple times.
#
# Usage: scripts/install-linux.sh [binary_path]
#   binary_path — absolute path to keymapperd (default: found via `which`).
# ---------------------------------------------------------------------------

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

UNIT_NAME="keymapperd.service"
SYSTEMD_USER_DIR="$HOME/.config/systemd/user"

# Find the unit template. It may be alongside the script (package layout)
# or under ../resources/systemd/ (repo layout).
if [ -f "$SCRIPT_DIR/resources/systemd/$UNIT_NAME" ]; then
    UNIT_TEMPLATE="$SCRIPT_DIR/resources/systemd/$UNIT_NAME"
elif [ -f "$SCRIPT_DIR/../resources/systemd/$UNIT_NAME" ]; then
    UNIT_TEMPLATE="$(cd "$SCRIPT_DIR/.." && pwd)/resources/systemd/$UNIT_NAME"
else
    echo "Error: systemd unit template not found near the script." >&2
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

# Ensure the systemd user directory exists.
mkdir -p "$SYSTEMD_USER_DIR"

# Copy the unit template and substitute placeholders.
sed "s|@BINARY_PATH@|$BINARY_PATH|g" "$UNIT_TEMPLATE" > "$SYSTEMD_USER_DIR/$UNIT_NAME"

echo "Installed ${UNIT_NAME} to ${SYSTEMD_USER_DIR}/"

# Reload the systemd user manager so it picks up the new unit.
systemctl --user daemon-reload

# Enable (start on login) and start the service now.
systemctl --user enable --now "$UNIT_NAME"

if systemctl --user is-active "$UNIT_NAME" >/dev/null 2>&1; then
    echo "keymapperd is running via systemd."
else
    echo "Warning: service was installed but does not appear to be running." >&2
    echo "Check with: systemctl --user status keymapperd.service" >&2
fi

# Desktop-specific active-window detection helpers. Both are optional: the
# daemon degrades gracefully without them, so a failure here is only a
# warning.
case "$(printf '%s' "${XDG_CURRENT_DESKTOP:-}" | tr '[:lower:]' '[:upper:]')" in
    *GNOME*)
        # Install the GNOME Shell extension that exposes the focused
        # window's application id to the daemon.
        if ! "$SCRIPT_DIR/install-gnome-ext.sh"; then
            echo "Warning: GNOME extension installation failed." >&2
            echo "Active-window detection on GNOME will be unavailable." >&2
        fi
        ;;
    *KDE*)
        # Install the KWin active-window script template. It lives outside
        # the daemon sandbox so KWin can read it; detection falls back to
        # the template embedded in the binary.
        if ! "$SCRIPT_DIR/install-kwin-script.sh"; then
            echo "Warning: KWin script installation failed." >&2
            echo "Active-window detection on KDE will use the embedded" >&2
            echo "script template instead." >&2
        fi
        ;;
esac
