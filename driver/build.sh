#!/bin/sh
# ---------------------------------------------------------------------------
# Builds the KeyMapperVirtualHID DriverKit extension.
#
# For local development the driver is built without code signing so it can be
# tested on the build machine. Ad-hoc code signing is not supported with the
# DriverKit SDK. For distribution a valid Apple Development or Developer ID
# certificate is required.
#
# Usage:
#   ./build.sh                    # Release build, both architectures
#   ./build.sh debug              # Debug build, active architecture only
#   ./build.sh clean              # Clean build artifacts
# ---------------------------------------------------------------------------

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${0}")" && pwd)"
PROJECT="${SCRIPT_DIR}/KeyMapperVirtualHID.xcodeproj"
SCHEME="KeyMapperVirtualHID"

case "${1:-}" in
    debug)
        xcodebuild \
            -project "${PROJECT}" \
            -scheme "${SCHEME}" \
            -configuration Debug \
            CODE_SIGNING_ALLOWED=NO \
            CODE_SIGNING_REQUIRED=NO \
            ONLY_ACTIVE_ARCH=YES
        ;;
    clean)
        xcodebuild \
            -project "${PROJECT}" \
            -scheme "${SCHEME}" \
            clean
        echo "Clean complete."
        exit 0
        ;;
    "")
        # Release build for both architectures. Code signing is disabled for
        # local development — the DriverKit SDK does not support ad-hoc signing.
        xcodebuild \
            -project "${PROJECT}" \
            -scheme "${SCHEME}" \
            -configuration Release \
            CODE_SIGNING_ALLOWED=NO \
            CODE_SIGNING_REQUIRED=NO \
            ARCHS="arm64 x86_64" \
            ONLY_ACTIVE_ARCH=NO
        ;;
    *)
        echo "Usage: $0 [debug|clean]" >&2
        exit 1
        ;;
esac

echo ""
echo "Build complete."
