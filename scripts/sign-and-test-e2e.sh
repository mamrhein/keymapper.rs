#!/bin/sh
# ---------------------------------------------------------------------------
# Installs the Karabiner DriverKit VirtualHIDDevice driver, signs test
# binaries with an ad-hoc signature, and runs e2e sandbox tests.
#
# On macOS, CGEventTap requires the calling process to be code-signed.
# Ad-hoc signing (codesign --sign -) is sufficient; no certificate needed.
# On other platforms this script skips signing and just runs the tests.
# ---------------------------------------------------------------------------

set -e

sign_macos() {
    # Find the e2e_tests test binary (the executable has no extension).
    bin=$(find target/debug/deps -maxdepth 1 -name 'e2e_tests-*' \
          ! -name '*.*' -type f 2>/dev/null | head -1)

    if [ -n "$bin" ]; then
        codesign --force --sign - "$bin"
    fi
}

setup_karabiner_driver() {
    # Install the Karabiner DriverKit VirtualHIDDevice package (the driver
    # through which keymapperd emits remapped keys) and verify the extension
    # is enabled.  The e2e tests need a live driver: without it the daemon
    # waits for the Karabiner socket and produces no output.
    if [ "$(id -u)" -ne 0 ]; then
        sudo scripts/install-karabiner-macos.sh
    else
        scripts/install-karabiner-macos.sh
    fi

    # Fail early if the extension is not enabled.  ("disabled" does not
    # contain "enabled", so the substring check is unambiguous.)
    if ! systemextensionsctl list 2>/dev/null \
            | grep -F "org.pqrs.Karabiner-DriverKit-VirtualHIDDevice" \
            | grep -q "enabled"; then
        echo "Error: the Karabiner DriverKit extension is not enabled." >&2
        echo "Enable it in: System Settings > General > Login Items &" >&2
        echo "Extensions > Driver Extensions, then re-run." >&2
        exit 1
    fi
}

# Build the daemon binary.  E2E tests spawn keymapperd as a subprocess and
# resolve it relative to their own location in target/debug/.  The `e2e`
# feature compiles the test hooks (readiness file, active-app override,
# capture mode) into the daemon so the harness can drive it.
cargo build --features e2e --bin keymapperd "$@"

# Build and sign the test binary without running it.
cargo nextest run --test e2e_tests --no-run "$@"

# Sign on macOS and install the Karabiner DriverKit driver.
if [ "$(uname -s)" = "Darwin" ]; then
    sign_macos
    setup_karabiner_driver
fi

# Run the tests.  On macOS, running as root bypasses TCC Accessibility
# permission checks required for CGEventTap creation.  CI=1 is set
# explicitly so the e2e tests run even when the caller's environment was
# sanitized by an outer `sudo` (which strips CI by default).
if [ "$(uname -s)" = "Darwin" ]; then
    sudo -E PATH="$PATH" $(which cargo) nextest run --no-capture --test e2e_tests "$@"
else
    cargo nextest run --no-capture --test e2e_tests "$@"
fi
