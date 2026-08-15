#!/bin/bash
# ---------------------------------------------------------------------------
# Creates a macOS DMG for keymapper distribution.
#
# Usage: scripts/package-macos.sh <version> [target]
#
# Arguments:
#   version  — Release tag without 'v' prefix (e.g. "0.1.0").
#   target   — Rust target triple (default: aarch64-apple-darwin).
#
# Prerequisites:
#   - Release binaries already built in target/<target>/release/
#   - DriverKit driver built at driver/Build/Products/Release/KeyMapperVirtualHID.kext
#   - macOS with hdiutil (built-in)
# ---------------------------------------------------------------------------

set -euo pipefail

if [ $# -lt 1 ]; then
    echo "usage: $0 <version> [target]"
    exit 1
fi

VERSION="$1"
TARGET="${2:-aarch64-apple-darwin}"
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

NAME="keymapper-${VERSION}-${TARGET}"
VOLUME_NAME="Install keymapper ${VERSION}"
DMG="${NAME}.dmg"

STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT

VOLUME_DIR="${STAGING}/${VOLUME_NAME}"
mkdir -p "${VOLUME_DIR}/bin"

# Copy binaries.
cp "${PROJECT_ROOT}/target/${TARGET}/release/keymapper" "${VOLUME_DIR}/bin/"
cp "${PROJECT_ROOT}/target/${TARGET}/release/keymapperd" "${VOLUME_DIR}/bin/"

# Copy documentation.
cp "${PROJECT_ROOT}/README.md" "${VOLUME_DIR}/"
cp "${PROJECT_ROOT}/LICENSE.TXT" "${VOLUME_DIR}/"

# Copy platform-specific docs.
mkdir -p "${VOLUME_DIR}/docs"
cp "${PROJECT_ROOT}/docs/macos-driver.md" "${VOLUME_DIR}/docs/"

# Copy the DriverKit virtual HID driver if it was prebuilt.
if [ -d "${PROJECT_ROOT}/driver/Build/Products/Release/KeyMapperVirtualHID.kext" ]; then
    mkdir -p "${VOLUME_DIR}/driver"
    cp -R "${PROJECT_ROOT}/driver/Build/Products/Release/KeyMapperVirtualHID.kext" \
       "${VOLUME_DIR}/driver/"
    echo "Included KeyMapperVirtualHID.kext in DMG."
else
    echo "Warning: DriverKit driver not found; DMG will not include the virtual HID driver."
    echo "         Run 'cd driver && make release' before packaging to include it."
fi

# Copy the launchd plist template and service install script.
mkdir -p "${VOLUME_DIR}/resources/launchd"
cp "${PROJECT_ROOT}/resources/launchd/de.adrhinum.keymapperd.plist" \
   "${VOLUME_DIR}/resources/launchd/"
cp "${PROJECT_ROOT}/scripts/install-macos.sh" "${VOLUME_DIR}/"
cp "${PROJECT_ROOT}/scripts/uninstall-macos.sh" "${VOLUME_DIR}/"

# Install script — copies binaries to a usable location and sets up the
# LaunchDaemon.  The daemon runs as root for IOKit device seizure.
cat > "${VOLUME_DIR}/install.sh" << 'INSTALL'
#!/bin/bash
# ---------------------------------------------------------------------------
# Installs keymapper binaries to /usr/local/bin (default) or a custom path,
# then registers the LaunchDaemon.
#
# The daemon runs as root to perform IOKit device seizure.  This script
# requires sudo privileges.
#
# Usage: ./install.sh [destination]
# ---------------------------------------------------------------------------

set -euo pipefail

DEST="${1:-/usr/local/bin}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Require root.
if [ "$EUID" -ne 0 ]; then
    echo "This script must be run as root (use sudo)." >&2
    exit 1
fi

if [ ! -d "$DEST" ]; then
    echo "Creating ${DEST}..."
    mkdir -p "$DEST"
fi

cp "${SCRIPT_DIR}/bin/keymapper" "$DEST/"
cp "${SCRIPT_DIR}/bin/keymapperd" "$DEST/"

echo "Installed keymapper to ${DEST}."
echo ""

# Install the DriverKit driver if it is bundled.
DRIVER_DIR="$HOME/Library/Extensions"
if [ -d "${SCRIPT_DIR}/driver/KeyMapperVirtualHID.kext" ]; then
    mkdir -p "$DRIVER_DIR"
    cp -R "${SCRIPT_DIR}/driver/KeyMapperVirtualHID.kext" "$DRIVER_DIR/"
    chown -R "$USER" "$DRIVER_DIR/KeyMapperVirtualHID.kext" 2>/dev/null || true
    echo "Installed virtual HID driver to ${DRIVER_DIR}/."
    echo "On first run, approve the driver in System Settings > Privacy & Security."
else
    echo "Error: No virtual HID driver found." >&2
    echo "Build and install the driver before running the installer:" >&2
    echo "  cd driver && make install" >&2
    exit 1
fi
fi

# Register the LaunchDaemon.
"${SCRIPT_DIR}/install-macos.sh" "${DEST}/keymapperd"

echo ""
echo "Next steps:"
echo "  keymapper config create   # create a configuration file"
INSTALL
chmod +x "${VOLUME_DIR}/install.sh"

# Write a friendly README for the DMG.
cat > "${VOLUME_DIR}/INSTALL.txt" << READMI
keymapper ${VERSION} for macOS
==============================

Quick install (requires sudo):
  sudo ./install.sh

Manual install:
  sudo cp bin/keymapper /usr/local/bin/
  sudo cp bin/keymapperd /usr/local/bin/
  sudo ./install-macos.sh /usr/local/bin/keymapperd

Virtual HID driver (recommended for reliable key emission):
  The bundled driver is installed automatically by install.sh.
  On first run, approve it in System Settings > Privacy & Security.
  See docs/macos-driver.md for troubleshooting.

Then:
  keymapper config create    # create a configuration file
  keymapper server status    # verify the daemon is running

To uninstall:
  sudo ./uninstall-macos.sh

Full documentation is in README.md.
READMI

# Create the compressed DMG.
#   -srcfolder : contents to include
#   -volname   : volume name shown when mounted
#   -format UDZO: zlib-compressed read-only DMG
#   -ov        : overwrite existing output
hdiutil create \
    -volname "${VOLUME_NAME}" \
    -srcfolder "${VOLUME_DIR}" \
    -format UDZO \
    -ov \
    "${PROJECT_ROOT}/${DMG}"

echo "Created ${PROJECT_ROOT}/${DMG}"
