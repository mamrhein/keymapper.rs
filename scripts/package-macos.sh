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

# Copy the launchd plist template and service install script.
mkdir -p "${VOLUME_DIR}/resources/launchd"
cp "${PROJECT_ROOT}/resources/launchd/de.adrhinum.keymapperd.plist" \
   "${VOLUME_DIR}/resources/launchd/"
cp "${PROJECT_ROOT}/scripts/install-macos.sh" "${VOLUME_DIR}/"
cp "${PROJECT_ROOT}/scripts/uninstall-macos.sh" "${VOLUME_DIR}/"

# Install script — copies binaries to a usable location and sets up launchd.
cat > "${VOLUME_DIR}/install.sh" << 'INSTALL'
#!/bin/bash
# ---------------------------------------------------------------------------
# Installs keymapper binaries to /usr/local/bin (default) or a custom path,
# then registers the launchd service.
#
# Usage: ./install.sh [destination]
# ---------------------------------------------------------------------------

set -euo pipefail

DEST="${1:-/usr/local/bin}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

if [ ! -d "$DEST" ]; then
    echo "Creating ${DEST}..."
    sudo mkdir -p "$DEST"
fi

sudo cp "${SCRIPT_DIR}/bin/keymapper" "$DEST/"
sudo cp "${SCRIPT_DIR}/bin/keymapperd" "$DEST/"

echo "Installed keymapper to ${DEST}."
echo ""

# Register the launchd service.
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

Quick install:
  Double-click install.sh (or run ./install.sh in Terminal).

Manual install:
  cp bin/keymapper /usr/local/bin/
  cp bin/keymapperd /usr/local/bin/
  ./install-macos.sh /usr/local/bin/keymapperd

Then:
  keymapper config create    # create a configuration file
  keymapper server status    # verify the daemon is running

To uninstall:
  ./uninstall-macos.sh

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
