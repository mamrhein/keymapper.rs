#!/bin/sh
# ---------------------------------------------------------------------------
# Builds the DriverKit virtual HID driver, signs test binaries with an
# ad-hoc signature, and runs e2e sandbox tests.
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

build_and_install_driver() {
    # Build the DriverKit virtual HID driver and install it to the user's
    # Extensions directory so it is discoverable via IOKit.  On GitHub Actions
    # runners, DriverKit extensions in ~/Library/Extensions are loaded
    # automatically without requiring user approval.  Use 'install-ci' to
    # disable code signing — DriverKit 25.5 rejects ad-hoc signing.
    cd driver
    make install-ci
    cd ..
}

# Build the daemon binary.  E2E tests spawn keymapperd as a subprocess and
# resolve it relative to their own location in target/debug/.
cargo build --bin keymapperd "$@"

# Build and sign the test binary without running it.
cargo nextest run --test e2e_tests --no-run "$@"

# Sign on macOS and install the virtual HID driver.
if [ "$(uname -s)" = "Darwin" ]; then
    sign_macos
    build_and_install_driver
fi

# Run the tests.  On macOS, running as root bypasses TCC Accessibility
# permission checks required for CGEventTap creation.
if [ "$(uname -s)" = "Darwin" ]; then
    sudo -E PATH="$PATH" $(which cargo) nextest run --no-capture --test e2e_tests "$@"
else
    cargo nextest run --no-capture --test e2e_tests "$@"
fi
