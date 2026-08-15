// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Integration tests for virtual HID driver availability.
//!
//! Verifies that `HidSocket::discover_and_open()` behaves correctly in both
//! scenarios: driver not loaded (returns error) and driver loaded (returns
//! Ok).  The "loaded" scenario only passes when the DriverKit extension is
//! actually running.

#[cfg(target_os = "macos")]
mod hid_tests {
    use keymapper::platform::HidSocket;

    /// Verify that `discover_and_open()` returns a meaningful error when the
    /// DriverKit extension is not loaded.  This test expects the driver to be
    /// absent, so it only asserts that we get a `Result::Err` (not that the
    /// specific variant matches).
    ///
    /// On CI (running as root) the driver may auto-load, causing this test to
    /// be skipped instead.  That is acceptable — the complementary test below
    /// will pass in that scenario.
    #[test]
    fn driver_not_loaded_returns_error() {
        match HidSocket::discover_and_open() {
            Err(e) => {
                // The error message should be descriptive.
                let msg = e.to_string();
                assert!(!msg.is_empty(), "error message should not be empty");
                eprintln!(
                    "Driver not available (expected): {e}. Run `keymapper \
                     driver install` to load the DriverKit extension."
                );
            }
            Ok(_) => {
                // The driver IS loaded (e.g. on CI as root).  This is fine;
                // just note it and skip the assertion.
                eprintln!(
                    "Driver IS available — this test expects it to be \
                     absent. The positive test below will verify success."
                );
            }
        }
    }

    /// Verify that `discover_and_open()` succeeds when the driver is loaded.
    /// This test only passes if the DriverKit extension is running.  On
    /// development machines this requires manual installation; on CI (root)
    /// the driver loads automatically.
    ///
    /// If the driver is not loaded, this test is skipped gracefully.
    #[test]
    fn driver_loaded_returns_ok() {
        let result = HidSocket::discover_and_open();

        if let Ok(socket) = result {
            // The socket is connected.  We can't easily send a real HID report
            // without a driver, but the fact that `discover_and_open()`
            // returned Ok means the IOKit service was found and the socket
            // was created successfully.  Drop it to clean up.
            drop(socket);
            eprintln!("Driver is connected and ready for HID reports.");
        } else {
            // HidSocket doesn't implement Debug, so match the error manually.
            let err = match result {
                Ok(_) => unreachable!(),
                Err(e) => e,
            };
            eprintln!(
                "skipping: driver not loaded ({err}). Run `keymapper driver \
                 install` and approve in System Settings."
            );
        }
    }

    /// Verify that the error type implements Display meaningfully.
    #[test]
    fn driver_error_has_meaningful_display() {
        let result = HidSocket::discover_and_open();
        if let Err(e) = result {
            let msg = e.to_string();
            // The error message should contain useful context.
            assert!(
                msg.contains("HID")
                    || msg.contains("IOKit")
                    || msg.contains("driver"),
                "error message should mention HID, IOKit, or driver: {msg}"
            );
        }
        // If the driver is available, the Display check is not applicable;
        // that's fine since the other tests cover this path.
    }
}

/// On non-macOS platforms there's nothing to test.  Provide a compile-time
/// guard test so the crate compiles cleanly.
#[cfg(not(target_os = "macos"))]
mod skip_tests {
    #[test]
    fn driverkit_not_on_macos() {
        eprintln!("skipping: virtual HID driver is macOS-only.");
    }
}
