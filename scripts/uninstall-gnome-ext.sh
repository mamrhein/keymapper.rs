#!/bin/bash
# ---------------------------------------------------------------------------
# Removes the keymapper GNOME Shell extension installed by
# scripts/install-gnome-ext.sh.
#
# Disables the extension, drops its uuid from the enabled-extensions
# setting, and deletes the extension directory. Idempotent — runs cleanly
# when the extension is not installed.
#
# Usage: scripts/uninstall-gnome-ext.sh
# ---------------------------------------------------------------------------

set -euo pipefail

# Must match the uuid in the extension's metadata.json.
EXTENSION_UUID="local.keymapper.active-window"
SCHEMA="org.gnome.shell"
ENABLED_KEY="enabled-extensions"

DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
EXT_DIR="$DATA_HOME/gnome-shell/extensions/$EXTENSION_UUID"

# Print the enabled extension uuids as a bare comma-separated list. Outputs
# nothing when gsettings is unavailable or the setting cannot be read (e.g.
# outside a GNOME session).
enabled_uuids() {
    command -v gsettings >/dev/null 2>&1 || return 0
    local current
    current="$(gsettings get "$SCHEMA" "$ENABLED_KEY" 2>/dev/null || true)"
    [ -n "$current" ] || return 0
    # `gsettings get` prints e.g. `@as ['a', 'b']` or `@as []`.
    current="${current#*[}" # drop the `@as [` prefix
    current="${current%]}"  # drop the trailing `]`
    current="${current//\'/}" # drop the quotes around the uuids
    current="${current// /}"  # normalize separators (uuids contain no spaces)
    printf '%s' "$current"
}

# Whether the extension is present anywhere we know of.
installed=false
[ -d "$EXT_DIR" ] && installed=true
if command -v gnome-extensions >/dev/null 2>&1 &&
   gnome-extensions show "$EXTENSION_UUID" >/dev/null 2>&1; then
    installed=true
fi
[[ ",$(enabled_uuids)," == *",$EXTENSION_UUID,"* ]] && installed=true
if [ "$installed" != true ]; then
    echo "Extension ${EXTENSION_UUID} is not installed."
    exit 0
fi

# Disable and uninstall via the CLI when the Shell knows the extension; both
# are best effort outside a GNOME session.
if command -v gnome-extensions >/dev/null 2>&1; then
    gnome-extensions disable "$EXTENSION_UUID" 2>/dev/null || true
    gnome-extensions uninstall "$EXTENSION_UUID" 2>/dev/null || true
fi

# Drop the uuid from enabled-extensions (install-gnome-ext.sh may have
# written it directly while the Shell did not know the extension yet).
inner="$(enabled_uuids)"
if [[ ",$inner," == *",$EXTENSION_UUID,"* ]]; then
    items=""
    uuids=()
    IFS=',' read -r -a uuids <<< "$inner"
    for uuid in "${uuids[@]}"; do
        case "$uuid" in
            "" | "$EXTENSION_UUID") continue ;;
        esac
        items+="'$uuid', "
    done
    items="${items%, }"
    if gsettings set "$SCHEMA" "$ENABLED_KEY" "[$items]" 2>/dev/null; then
        echo "Removed ${EXTENSION_UUID} from ${SCHEMA} ${ENABLED_KEY}."
    fi
fi

# Remove the extension directory (the CLI uninstall above already did when
# the Shell knew the extension).
if [ -d "$EXT_DIR" ]; then
    rm -rf "$EXT_DIR"
    echo "Removed ${EXT_DIR}/"
fi

echo "Uninstalled ${EXTENSION_UUID}."
