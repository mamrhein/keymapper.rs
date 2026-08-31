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
#   - Network access (downloads the pinned Karabiner DriverKit package)
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

# Pinned Karabiner DriverKit VirtualHIDDevice release.  Keep in sync with
# scripts/install-karabiner-macos.sh and brew/karabiner-driverkit-virtualhiddevice.rb.
KARABINER_VERSION="8.2.0"
KARABINER_PKG_NAME="Karabiner-DriverKit-VirtualHIDDevice-${KARABINER_VERSION}.pkg"
KARABINER_PKG_SHA256="7faf4c33046c2274726da9e29da795fb2d2ad81796557db0fcc1686c611eeafc"
KARABINER_PKG_URL="https://github.com/pqrs-org/Karabiner-DriverKit-VirtualHIDDevice/releases/download/v${KARABINER_VERSION}/${KARABINER_PKG_NAME}"

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

# Download and bundle the pinned Karabiner DriverKit package (the driver
# through which keymapperd emits remapped keys).
mkdir -p "${VOLUME_DIR}/karabiner"
echo "Downloading ${KARABINER_PKG_NAME} (pinned release v${KARABINER_VERSION})..."
curl -fL --retry 3 -o "${STAGING}/${KARABINER_PKG_NAME}" "$KARABINER_PKG_URL"
echo "${KARABINER_PKG_SHA256}  ${STAGING}/${KARABINER_PKG_NAME}" | shasum -a 256 --check
cp "${STAGING}/${KARABINER_PKG_NAME}" "${VOLUME_DIR}/karabiner/"
echo "Included ${KARABINER_PKG_NAME} in DMG."

# Copy the launchd plist templates and service install scripts.
mkdir -p "${VOLUME_DIR}/resources/launchd"
cp "${PROJECT_ROOT}/resources/launchd/de.adrhinum.keymapperd.plist" \
   "${VOLUME_DIR}/resources/launchd/"
cp "${PROJECT_ROOT}/resources/launchd/org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon.plist" \
   "${VOLUME_DIR}/resources/launchd/"
cp "${PROJECT_ROOT}/scripts/install-macos.sh" "${VOLUME_DIR}/"
cp "${PROJECT_ROOT}/scripts/uninstall-macos.sh" "${VOLUME_DIR}/"
cp "${PROJECT_ROOT}/scripts/install-karabiner-macos.sh" "${VOLUME_DIR}/"
cp "${PROJECT_ROOT}/scripts/uninstall-karabiner-macos.sh" "${VOLUME_DIR}/"

# Install script — copies binaries to a usable location, registers the
# LaunchDaemon, and installs the Karabiner DriverKit driver.  The daemon runs
# as root for IOKit device seizure.
cat > "${VOLUME_DIR}/install.sh" << 'INSTALL'
#!/bin/bash
# ---------------------------------------------------------------------------
# Installs keymapper binaries to /usr/local/bin (default) or a custom path,
# then registers the LaunchDaemon and installs the Karabiner DriverKit
# VirtualHIDDevice driver (the device through which keymapperd emits
# remapped keys).
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

# Install the Karabiner DriverKit package.  The DMG bundles the pinned
# package; if it is missing, install-karabiner-macos.sh falls back to a
# pinned download from the pqrs GitHub releases.
KARABINER_PKG=""
for candidate in "${SCRIPT_DIR}"/karabiner/*.pkg; do
    if [ -f "$candidate" ]; then
        KARABINER_PKG="$candidate"
        break
    fi
done

if [ -n "$KARABINER_PKG" ]; then
    "${SCRIPT_DIR}/install-macos.sh" "${DEST}/keymapperd" "$KARABINER_PKG"
else
    echo "Warning: no bundled Karabiner package found; it will be downloaded." >&2
    "${SCRIPT_DIR}/install-macos.sh" "${DEST}/keymapperd"
fi

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

Virtual HID driver:
  The Karabiner DriverKit VirtualHIDDevice package (bundled in the
  karabiner/ directory) is installed automatically by install.sh.
  On first run, the driver extension may need to be enabled once in:
  System Settings > General > Login Items & Extensions > Driver Extensions.
  No reboot is required.  See docs/macos-driver.md for troubleshooting.

Then:
  keymapper config create    # create a configuration file
  keymapper daemon status    # verify the daemon is running

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
