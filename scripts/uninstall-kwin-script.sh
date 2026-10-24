#!/bin/bash
# ---------------------------------------------------------------------------
# Removes the keymapper KWin active-window script installed by
# scripts/install-kwin-script.sh.
#
# Idempotent — runs cleanly when the script is not installed.
#
# Usage: scripts/uninstall-kwin-script.sh
# ---------------------------------------------------------------------------

set -euo pipefail

DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
DEST_DIR="$DATA_HOME/keymapperd/kwin"
DEST="$DEST_DIR/active-window.js"

if [ -f "$DEST" ]; then
    rm "$DEST"
    echo "Removed ${DEST}"
fi
# Drop the directory when nothing else lives in it.
rmdir "$DEST_DIR" 2>/dev/null || true
