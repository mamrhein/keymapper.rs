#!/bin/bash
# ---------------------------------------------------------------------------
# Installs the keymapper GNOME Shell extension.
#
# The extension exports the focused window's application id over a dedicated
# D-Bus method, which the keymapperd daemon queries on GNOME. Files are copied
# to $XDG_DATA_HOME/gnome-shell/extensions/<uuid>/ and the extension is
# enabled. Idempotent — safe to run multiple times.
#
# Note on activation timing: GNOME ≥ 45 scans the extensions directories only
# at session start. A freshly installed extension therefore becomes active at
# the next login; the daemon falls back gracefully until then.
#
# Usage: scripts/install-gnome-ext.sh
#
# To uninstall:
#   gnome-extensions uninstall local.keymapper.active-window
# ---------------------------------------------------------------------------

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Must match the uuid in the extension's metadata.json.
EXTENSION_UUID="local.keymapper.active-window"
SCHEMA="org.gnome.shell"
ENABLED_KEY="enabled-extensions"

# Find the extension sources. They may be alongside the script (package
# layout) or under ../resources/gnome-ext/ (repo layout).
if [ -d "$SCRIPT_DIR/resources/gnome-ext" ]; then
    EXT_SRC="$SCRIPT_DIR/resources/gnome-ext"
elif [ -d "$SCRIPT_DIR/../resources/gnome-ext" ]; then
    EXT_SRC="$(cd "$SCRIPT_DIR/.." && pwd)/resources/gnome-ext"
else
    echo "Error: GNOME extension sources not found near the script." >&2
    exit 1
fi

DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
EXT_DIR="$DATA_HOME/gnome-shell/extensions/$EXTENSION_UUID"

# Copy the extension files (idempotent overwrite picks up upgrades).
mkdir -p "$EXT_DIR"
cp "$EXT_SRC"/* "$EXT_DIR/"
echo "Installed extension files to ${EXT_DIR}/"

# Enable the extension. The CLI only knows extensions the running Shell has
# already scanned; for a fresh install it fails, so the uuid is appended to
# the enabled-extensions setting directly and the Shell loads it at the next
# login.
enable_extension() {
    if command -v gnome-extensions >/dev/null 2>&1 &&
       gnome-extensions enable "$EXTENSION_UUID" 2>/dev/null; then
        echo "Enabled ${EXTENSION_UUID}."
        return 0
    fi
    if ! command -v gsettings >/dev/null 2>&1; then
        echo "Warning: neither gnome-extensions nor gsettings is available." >&2
        echo "The extension files are installed; enable them manually." >&2
        return 0
    fi
    # `gsettings get` prints e.g. `@as ['a', 'b']` or `@as []`.
    local current inner items="" uuid updated
    current="$(gsettings get "$SCHEMA" "$ENABLED_KEY" 2>/dev/null || true)"
    [ -n "$current" ] || return 0
    inner="${current#*[}"   # drop the `@as [` prefix
    inner="${inner%]}"      # drop the trailing `]`
    inner="${inner//\'/}"   # drop the quotes around the uuids
    inner="${inner// /}"    # normalize separators (uuids contain no spaces)
    if [[ ",$inner," == *",$EXTENSION_UUID,"* ]]; then
        return 0 # already enabled
    fi
    local -a uuids=()
    IFS=',' read -r -a uuids <<< "$inner"
    for uuid in "${uuids[@]}"; do
        # Skip empty entries (an empty list yields a single empty element).
        case "$uuid" in
            "") continue ;;
        esac
        items+="'$uuid', "
    done
    updated="[${items}'$EXTENSION_UUID']"
    if gsettings set "$SCHEMA" "$ENABLED_KEY" "$updated" 2>/dev/null; then
        echo "Added ${EXTENSION_UUID} to ${SCHEMA} ${ENABLED_KEY}."
        echo "The Shell loads it at the next login (GNOME ≥ 45 scans only at" \
             "session start)."
    fi
}

enable_extension
