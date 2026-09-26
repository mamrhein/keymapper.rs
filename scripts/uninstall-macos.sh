#!/bin/bash
# ---------------------------------------------------------------------------
# Uninstalls the keymapper services from macOS.
#
# Boots out the virtkbdd LaunchDaemon (system domain) and the keymapperd
# LaunchAgent (console user's gui domain) and removes their plists.  Does not
# delete log files or binaries. It also removes the Karabiner DriverKit
# VirtualHIDDevice package (deactivate driver + remove files) via
# uninstall-karabiner-macos.sh. Requires sudo privileges.
# ---------------------------------------------------------------------------

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

KEYMAPPERD_LABEL="de.adrhinum.keymapperd"
VIRTKBDD_LABEL="de.adrhinum.virtkbdd"

# Require root.
if [ "$EUID" -ne 0 ]; then
    echo "This script must be run as root (use sudo)." >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# virtkbdd — root LaunchDaemon (system domain)
# ---------------------------------------------------------------------------

if launchctl print system/"$VIRTKBDD_LABEL" >/dev/null 2>&1; then
    launchctl bootout system/"$VIRTKBDD_LABEL"
    echo "Stopped ${VIRTKBDD_LABEL}."
else
    echo "${VIRTKBDD_LABEL} is not loaded."
fi

VIRTKBDD_PLIST="/Library/LaunchDaemons/${VIRTKBDD_LABEL}.plist"
if [ -f "$VIRTKBDD_PLIST" ]; then
    rm "$VIRTKBDD_PLIST"
    echo "Removed ${VIRTKBDD_PLIST}"
else
    echo "No plist found at ${VIRTKBDD_PLIST}"
fi

# ---------------------------------------------------------------------------
# keymapperd — user LaunchAgent (gui/<UID> domain)
# ---------------------------------------------------------------------------

CONSOLE_USER="$(stat -f '%Su' /dev/console)"
CONSOLE_UID="$(id -u "$CONSOLE_USER")"
CONSOLE_HOME="$(dscl . -read "/Users/${CONSOLE_USER}" NFSHomeDirectory | awk '{print $2}')"

# Run a launchctl command in the console user's gui domain. This script runs
# as root (brew invokes it with sudo), and a root process cannot reach another
# user's gui/<UID> domain directly — the gui-domain verbs fail with an input/
# output error. `launchctl asuser` establishes the proper bootstrap port for
# that user's domain, so they succeed.
gui_launchctl() {
    launchctl asuser "$CONSOLE_UID" launchctl "$@"
}

if [ -n "$CONSOLE_HOME" ] && [ -d "$CONSOLE_HOME" ]; then
    KEYMAPPERD_PLIST="${CONSOLE_HOME}/Library/LaunchAgents/${KEYMAPPERD_LABEL}.plist"

    # The gui-domain verbs go through `gui_launchctl` (see above) so they run
    # in the console user's domain rather than root's.
    if gui_launchctl print "gui/${CONSOLE_UID}/${KEYMAPPERD_LABEL}" >/dev/null 2>&1; then
        gui_launchctl bootout "gui/${CONSOLE_UID}/${KEYMAPPERD_LABEL}"
        echo "Stopped ${KEYMAPPERD_LABEL}."
    else
        echo "${KEYMAPPERD_LABEL} is not loaded."
    fi

    if [ -f "$KEYMAPPERD_PLIST" ]; then
        rm "$KEYMAPPERD_PLIST"
        echo "Removed ${KEYMAPPERD_PLIST}"
    else
        echo "No plist found at ${KEYMAPPERD_PLIST}"
    fi
else
    echo "No console user found; skipping the keymapperd LaunchAgent." >&2
fi

# Remove a keymapperd LaunchDaemon left over from older releases (the daemon
# moved to the user domain).
LEGACY_PLIST="/Library/LaunchDaemons/${KEYMAPPERD_LABEL}.plist"
if [ -f "$LEGACY_PLIST" ]; then
    if launchctl print system/"$KEYMAPPERD_LABEL" >/dev/null 2>&1; then
        launchctl bootout system/"$KEYMAPPERD_LABEL" 2>/dev/null || true
    fi
    rm "$LEGACY_PLIST"
    echo "Removed legacy ${LEGACY_PLIST}"
fi

# Remove the Karabiner DriverKit package (deactivate driver + remove files).
echo ""
"${SCRIPT_DIR}/uninstall-karabiner-macos.sh"
