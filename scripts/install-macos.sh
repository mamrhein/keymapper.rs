#!/bin/bash
# ---------------------------------------------------------------------------
# Installs the keymapperd launchd service on macOS.
#
# Copies the plist template to ~/Library/LaunchAgents/, resolves the binary
# path, and boots the service.  Idempotent — safe to run multiple times.
#
# Usage: scripts/install-macos.sh [binary_path]
#   binary_path — absolute path to keymapperd (default: found via `which`).
# ---------------------------------------------------------------------------

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

LABEL="de.adrhinum.keymapperd"
LAUNCH_AGENTS_DIR="$HOME/Library/LaunchAgents"
LOG_DIR="$HOME/Library/Logs/keymapperd"

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

# Ensure the LaunchAgents directory exists.
mkdir -p "$LAUNCH_AGENTS_DIR"

# If the service is already loaded, unload it first so we can replace the plist.
if launchctl print gui/"$UID" "$LABEL" >/dev/null 2>&1; then
    launchctl bootout gui/"$UID" "$LABEL" 2>/dev/null || true
fi

# Copy the plist template and substitute placeholders.
mkdir -p "$LOG_DIR"
sed \
    -e "s|@BINARY_PATH@|$BINARY_PATH|g" \
    -e "s|@LOG_DIR@|$LOG_DIR|g" \
    "$PLIST_TEMPLATE" > "$LAUNCH_AGENTS_DIR/${LABEL}.plist"

echo "Installed ${LABEL}.plist to ${LAUNCH_AGENTS_DIR}/"

# Boot the service.
launchctl bootstrap gui/"$UID" "$LAUNCH_AGENTS_DIR/${LABEL}.plist"

if launchctl print gui/"$UID" "$LABEL" >/dev/null 2>&1; then
    echo "keymapperd is running via launchd."
else
    echo "Warning: service was installed but does not appear to be running." >&2
    echo "Check logs at ${LOG_DIR}/keymapperd-err.log" >&2
fi
