#!/bin/bash
# ---------------------------------------------------------------------------
# Installs the Karabiner DriverKit VirtualHIDDevice package on macOS.
#
# keymapperd emits remapped keys through the Karabiner DriverKit virtual HID
# driver.  This script:
#   1. installs the Karabiner package (from an explicit pkg path, a pkg
#      bundled next to the script, or a pinned download from the pqrs
#      GitHub releases),
#   2. activates the DriverKit extension (a one-time user approval in
#      System Settings may still be required),
#   3. registers the Karabiner daemon LaunchDaemon (Interactive, KeepAlive).
#
# Idempotent — safe to run multiple times.  Requires sudo privileges.
#
# Usage: scripts/install-karabiner-macos.sh [pkg_path]
#   pkg_path — path to the Karabiner .pkg (default: bundled next to the
#              script, or the pinned release downloaded from GitHub).
# ---------------------------------------------------------------------------

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Pinned Karabiner DriverKit VirtualHIDDevice release.  Keep in sync with
# scripts/package-macos.sh and brew/karabiner-driverkit-virtualhiddevice.rb.
KARABINER_VERSION="8.2.0"
KARABINER_PKG_NAME="Karabiner-DriverKit-VirtualHIDDevice-${KARABINER_VERSION}.pkg"
KARABINER_PKG_SHA256="7faf4c33046c2274726da9e29da795fb2d2ad81796557db0fcc1686c611eeafc"
KARABINER_PKG_URL="https://github.com/pqrs-org/Karabiner-DriverKit-VirtualHIDDevice/releases/download/v${KARABINER_VERSION}/${KARABINER_PKG_NAME}"

# Fixed install locations (the package always installs to these paths).
KARABINER_APP_DIR="/Library/Application Support/org.pqrs/Karabiner-DriverKit-VirtualHIDDevice"
MANAGER_BIN="/Applications/.Karabiner-VirtualHIDDevice-Manager.app/Contents/MacOS/Karabiner-VirtualHIDDevice-Manager"
BUNDLE_ID="org.pqrs.Karabiner-DriverKit-VirtualHIDDevice"

LABEL="org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon"
LAUNCH_DAEMONS_DIR="/Library/LaunchDaemons"

# Find the plist template.  It may be alongside the script (DMG layout) or
# under ../resources/launchd/ (repo layout).  The template needs no
# substitution: the package always installs the daemon to the same path.
if [ -f "$SCRIPT_DIR/resources/launchd/${LABEL}.plist" ]; then
    PLIST_TEMPLATE="$SCRIPT_DIR/resources/launchd/${LABEL}.plist"
elif [ -f "$SCRIPT_DIR/../resources/launchd/${LABEL}.plist" ]; then
    PLIST_TEMPLATE="$(cd "$SCRIPT_DIR/.." && pwd)/resources/launchd/${LABEL}.plist"
else
    echo "Error: launchd plist template not found near the script." >&2
    exit 1
fi

# Require root — the installer, launchctl, and systemextensionsctl all need it.
if [ "$EUID" -ne 0 ]; then
    echo "This script must be run as root (use sudo)." >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 1. Install the package (skip if already installed)
# ---------------------------------------------------------------------------

if [ -d "$KARABINER_APP_DIR" ]; then
    echo "Karabiner DriverKit package already installed."
else
    # Resolve the pkg: explicit argument, then a pkg bundled next to the
    # script (DMG layout), then a pinned download.
    PKG_PATH=""
    if [ $# -ge 1 ]; then
        PKG_PATH="$1"
    else
        for candidate in "$SCRIPT_DIR"/karabiner/*.pkg; do
            if [ -f "$candidate" ]; then
                PKG_PATH="$candidate"
                break
            fi
        done
    fi

    if [ -z "$PKG_PATH" ]; then
        echo "Downloading ${KARABINER_PKG_NAME} (pinned release v${KARABINER_VERSION})..."
        TMP_DIR="$(mktemp -d)"
        trap 'rm -rf "$TMP_DIR"' EXIT
        PKG_PATH="${TMP_DIR}/${KARABINER_PKG_NAME}"
        curl -fL --retry 3 -o "$PKG_PATH" "$KARABINER_PKG_URL"
        # Verify the download against the pinned checksum.
        echo "${KARABINER_PKG_SHA256}  ${PKG_PATH}" | shasum -a 256 --check
    fi

    if [ ! -f "$PKG_PATH" ]; then
        echo "Error: Karabiner package not found at '${PKG_PATH}'." >&2
        exit 1
    fi

    echo "Installing ${KARABINER_PKG_NAME}..."
    installer -pkg "$PKG_PATH" -target /
fi

# ---------------------------------------------------------------------------
# 2. Activate the DriverKit extension
# ---------------------------------------------------------------------------

if [ ! -x "$MANAGER_BIN" ]; then
    echo "Error: manager binary not found at ${MANAGER_BIN}." >&2
    exit 1
fi

echo "Activating the DriverKit extension..."
if ! "$MANAGER_BIN" activate; then
    echo "Warning: 'activate' returned an error; checking the driver state." >&2
fi

# ---------------------------------------------------------------------------
# 3. Register the Karabiner daemon LaunchDaemon
# ---------------------------------------------------------------------------

mkdir -p "$LAUNCH_DAEMONS_DIR"

# If the service is already loaded, unload it first so we can replace the plist.
if launchctl print system/"$LABEL" >/dev/null 2>&1; then
    launchctl bootout system/"$LABEL" 2>/dev/null || true
fi

cp "$PLIST_TEMPLATE" "$LAUNCH_DAEMONS_DIR/${LABEL}.plist"
chown root:wheel "$LAUNCH_DAEMONS_DIR/${LABEL}.plist"
chmod 644 "$LAUNCH_DAEMONS_DIR/${LABEL}.plist"

launchctl bootstrap system "$LAUNCH_DAEMONS_DIR/${LABEL}.plist"
echo "Installed ${LABEL}.plist to ${LAUNCH_DAEMONS_DIR}/"

# ---------------------------------------------------------------------------
# 4. Report the driver state
# ---------------------------------------------------------------------------

# The state column of `systemextensionsctl list` reads e.g. "activated
# enabled" or "activated disabled"; a missing line means the extension is
# not registered at all.  ("disabled" does not contain "enabled", so the
# substring check is unambiguous.)
STATE_LINE="$(systemextensionsctl list 2>/dev/null | grep -F "$BUNDLE_ID" || true)"

if [ -z "$STATE_LINE" ]; then
    echo "Warning: the DriverKit extension is not registered." >&2
    echo "Run '${MANAGER_BIN} activate' and check the system log:" >&2
    echo "  log show --predicate 'subsystem == \"com.apple.systemextensions\"' --last 10m" >&2
elif echo "$STATE_LINE" | grep -q "enabled"; then
    echo "DriverKit extension is active."
else
    echo ""
    echo "The DriverKit extension is registered but not yet enabled."
    echo "Enable it once in: System Settings > General > Login Items &"
    echo "Extensions > Driver Extensions (toggle on the Karabiner entry)."
    echo "No reboot is required."
fi
