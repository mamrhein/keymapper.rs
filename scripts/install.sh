#!/bin/bash
# ---------------------------------------------------------------------------
# Installs the precompiled keymapper binaries on Linux.
#
# Downloads the release archive for the current architecture from GitHub,
# installs keymapper and keymapperd into the install directory, and
# registers the keymapperd systemd user service.  No root is required.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/mamrhein/keymapper.rs/main/scripts/install.sh | bash
#
# Environment:
#   KEYMAPPER_VERSION     — release version to install (default: latest).
#   KEYMAPPER_INSTALL_DIR — directory for the binaries (default: ~/.local/bin).
# ---------------------------------------------------------------------------

set -euo pipefail

REPO="mamrhein/keymapper.rs"
INSTALL_DIR="${KEYMAPPER_INSTALL_DIR:-$HOME/.local/bin}"

# The systemd user service belongs to a regular user session; running as
# root would install into /root and talk to the wrong (or no) user bus.
if [ "$(id -u)" -eq 0 ]; then
    echo "Error: do not run this script as root. Run it as your regular user." >&2
    exit 1
fi

# Only the Linux binaries are installed here.  On Apple Silicon, uname -m
# reports arm64, which would otherwise map to the aarch64 Linux target.
if [ "$(uname -s)" != "Linux" ]; then
    echo "Error: this script installs the Linux binaries." >&2
    echo "On macOS, use: brew install --cask mamrhein/keymapper/keymapper-bin" >&2
    exit 1
fi

# Map the machine architecture to the release target triple.
case "$(uname -m)" in
    x86_64)        TARGET="x86_64-unknown-linux-gnu" ;;
    aarch64|arm64) TARGET="aarch64-unknown-linux-gnu" ;;
    *)
        echo "Error: unsupported architecture '$(uname -m)'." >&2
        exit 1
        ;;
esac

# Fetch a url to stdout, preferring curl and falling back to wget.
fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO - "$1"
    else
        echo "Error: curl or wget is required to download the release." >&2
        exit 1
    fi
}

# Resolve the version: pinned via KEYMAPPER_VERSION, or the latest release.
VERSION="${KEYMAPPER_VERSION:-}"
if [ -z "$VERSION" ]; then
    VERSION=$(fetch "https://api.github.com/repos/${REPO}/releases/latest" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
fi
VERSION="${VERSION#v}"

if [ -z "$VERSION" ]; then
    echo "Error: could not determine the release version." >&2
    exit 1
fi

URL="https://github.com/${REPO}/releases/download/v${VERSION}/keymapper-v${VERSION}-${TARGET}.tar.xz"
echo "Downloading keymapper v${VERSION} (${TARGET})..."

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

fetch "$URL" > "$TMP_DIR/keymapper.tar.xz"
tar -xJf "$TMP_DIR/keymapper.tar.xz" -C "$TMP_DIR"

# The archive extracts to dist/keymapper/v<version>/.
STAGING="$TMP_DIR/dist/keymapper/v${VERSION}"
if [ ! -d "$STAGING" ]; then
    echo "Error: unexpected archive layout (missing dist/keymapper/v${VERSION})." >&2
    exit 1
fi

# Install the binaries.
mkdir -p "$INSTALL_DIR"
install -m 755 "$STAGING/keymapper" "$STAGING/keymapperd" "$INSTALL_DIR/"
echo "Installed keymapper and keymapperd to ${INSTALL_DIR}/"

# Register the systemd user service (idempotent).  This can fail when there
# is no user session yet (e.g. over SSH); the binaries are installed either
# way, so warn instead of aborting.
if ! "$STAGING/install-linux.sh" "$INSTALL_DIR/keymapperd"; then
    echo "" >&2
    echo "Warning: the keymapperd service could not be started right now" >&2
    echo "(this can happen over SSH or before a graphical login)." >&2
    echo "Start it later from your desktop session with:" >&2
    echo "  keymapper daemon start" >&2
fi

# Hint if the install directory is not on the PATH.
case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        echo "Note: ${INSTALL_DIR} is not in your PATH. Add it to your shell profile." >&2
        ;;
esac

echo "Done. Manage the daemon with: keymapper daemon status | start | stop"
