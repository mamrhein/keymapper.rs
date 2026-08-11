#!/bin/sh
# ---------------------------------------------------------------------------
# Signs test binaries with an ad-hoc signature and runs e2e sandbox tests.
#
# On macOS, CGEventTap requires the calling process to be code-signed.
# Ad-hoc signing (codesign --sign -) is sufficient; no certificate needed.
# On other platforms this script skips signing and just runs the tests.
# ---------------------------------------------------------------------------

set -e

sign_macos() {
    # Find the e2e_sandbox test binary (the executable has no extension).
    bin=$(find target/debug/deps -maxdepth 1 -name 'e2e_sandbox-*' \
          ! -name '*.*' -type f 2>/dev/null | head -1)

    if [ -n "$bin" ]; then
        codesign --force --sign - "$bin"
    fi
}

# Build the daemon binary.  E2E tests spawn keymapperd as a subprocess and
# resolve it relative to their own location in target/debug/.
cargo build --bin keymapperd "$@"

# Build and sign the test binary without running it.
cargo nextest run --test e2e_sandbox --no-run "$@"

# Sign on macOS.
if [ "$(uname -s)" = "Darwin" ]; then
    sign_macos
fi

# Run the tests.
cargo nextest run --no-capture --test e2e_sandbox "$@"
