#!/bin/bash
# ---------------------------------------------------------------------------
# Builds and installs the keymapper utility and services on macOS.
# ---------------------------------------------------------------------------

set -euo pipefail

# build binaries
cargo build -r

# prep staging area
VERSION=$(cargo pkgid | cut -d "@" -f2)
STAGING="dist/keymapper/$VERSION"
mkdir -p "$STAGING/resources/launchd"
cp "target/release/keymapperd" "$STAGING/"
cp "target/release/virtkbdd" "$STAGING/"
cp resources/launchd/de.adrhinum.keymapperd.plist "$STAGING/resources/launchd/"
cp resources/launchd/de.adrhinum.virtkbdd.plist "$STAGING/resources/launchd/"
cp resources/launchd/org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon.plist "$STAGING/resources/launchd/"
cp scripts/install-macos.sh "$STAGING/"
cp scripts/install-karabiner-macos.sh "$STAGING/"

# Copy utility to homebrew location
UTIL_TARGET_DIR="/usr/local/bin"
cp "target/release/keymapper" "$UTIL_TARGET_DIR/"

# Install services
sudo $STAGING/install-macos.sh ${STAGING}/keymapperd ${STAGING}/virtkbdd
