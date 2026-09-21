#!/bin/sh
# ---------------------------------------------------------------------------
# Installs the Karabiner DriverKit VirtualHIDDevice driver, signs the test
# binary with an ad-hoc signature, and runs the e2e tests as root.
#
# The tests run in ci mode (CI=true): the harness spawns and stops its own
# keymapperd, so any already-running daemon is stopped first.  The user's
# config is backed up and restored on exit, ownership included.
#
# Root is required twice over: the key injector talks to the Karabiner
# DriverKit daemon's service socket, which only root may open, and running
# as root bypasses the TCC Accessibility permission checks required for
# the daemon's CGEventTap creation.  Ad-hoc signing (codesign --sign -) is
# sufficient for the test binary; no certificate needed.
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

# Build the daemon binary the harness spawns as a subprocess, resolving it
# relative to its own location in target/debug/.  The daemon is a plain
# production build; the harness drives it and verifies its decisions from
# its own debug log.
cargo build --bin keymapperd

# Build and sign the test binary without running it.
cargo nextest run --test e2e_tests --no-run
bin=$(find target/debug/deps -maxdepth 1 -name 'e2e_tests-*' \
      ! -name '*.*' -type f 2>/dev/null | head -1)
if [ -n "$bin" ]; then
    codesign --force --sign - "$bin"
fi

# Run the tests in ci mode (the harness spawns and stops its own daemon).
sudo -E env CI=true PATH="$PATH" $(which cargo) nextest run --no-capture \
     --test e2e_tests
