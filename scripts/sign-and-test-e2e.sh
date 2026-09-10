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

# Install the Karabiner DriverKit VirtualHIDDevice package (the driver
# through which keymapperd emits remapped keys) and verify the extension
# is enabled.  The e2e tests need a live driver: without it the daemon
# waits for the Karabiner socket and produces no output.
sudo scripts/install-karabiner-macos.sh

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

# Build the binaries the harness spawns as subprocesses, resolving each
# relative to its own location in target/debug/.  The daemon is a plain
# production build; the harness drives it, focuses a window with the test
# helper, and observes its output through the monitor.
cargo build --bin keymapperd --bin keymapper_monitor --bin keymapper_testwindow

# Build and sign the test binary without running it.
cargo nextest run --test e2e_tests --no-run
bin=$(find target/debug/deps -maxdepth 1 -name 'e2e_tests-*' \
      ! -name '*.*' -type f 2>/dev/null | head -1)
if [ -n "$bin" ]; then
    codesign --force --sign - "$bin"
fi

# Run the tests. Running as root bypasses TCC Accessibility permission checks
# required for CGEventTap creation.
sudo -E PATH="$PATH" $(which cargo) nextest run --no-capture --test e2e_tests
