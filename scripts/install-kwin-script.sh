#!/bin/bash
# ---------------------------------------------------------------------------
# Installs the keymapper KWin active-window script.
#
# keymapperd determines the focused window on KDE by loading a tiny
# JavaScript file through KWin's scripting interface. This script installs
# the canonical template of that file to
# $XDG_DATA_HOME/keymapperd/kwin/active-window.js, where the daemon and the
# CLI pick it up on every query; an identical copy embedded in the binaries
# is used as fallback when the file is not installed.
#
# The file is a template, not a persistent KWin package: KWin scripts cannot
# export D-Bus methods or emit signals, so a script loaded once could never
# answer queries. keymapperd substitutes the reply destination placeholder
# and stages a per-query copy at runtime. Installing the template here keeps
# it inspectable and upgradable outside of cargo.
#
# Idempotent — safe to run multiple times; re-running picks up upgrades and
# takes effect on the next active-window query (no restart needed).
#
# Usage: scripts/install-kwin-script.sh
# ---------------------------------------------------------------------------

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Find the script template. It may be alongside the script (package layout)
# or under ../resources/kwin-script/ (repo layout).
if [ -f "$SCRIPT_DIR/resources/kwin-script/active-window.js" ]; then
    TEMPLATE_SRC="$SCRIPT_DIR/resources/kwin-script/active-window.js"
elif [ -f "$SCRIPT_DIR/../resources/kwin-script/active-window.js" ]; then
    TEMPLATE_SRC="$(cd "$SCRIPT_DIR/.." && pwd)/resources/kwin-script/active-window.js"
else
    echo "Error: KWin script template not found near the script." >&2
    exit 1
fi

DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
DEST_DIR="$DATA_HOME/keymapperd/kwin"
DEST="$DEST_DIR/active-window.js"

# Copy the template (idempotent overwrite picks up upgrades).
mkdir -p "$DEST_DIR"
cp "$TEMPLATE_SRC" "$DEST"
echo "Installed KWin active-window script template to ${DEST}"
