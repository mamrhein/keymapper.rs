// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Windows reader setup: the harness spawns this binary with
//! `CREATE_NEW_CONSOLE` and brings its console window to the foreground, so
//! the reader's stdin is that console and it is the focused window.  The
//! daemon re-emits keys via `SendInput`, which the system delivers to the
//! foreground window — i.e. here.  Setup only needs to disable line input and
//! echo so each keystroke is available immediately instead of being buffered
//! until Enter.

use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, GetConsoleMode,
    GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
};

/// Disable line input and echo on the reader's console stdin.
pub(super) fn setup() {
    // Safety: GetStdHandle/GetConsoleMode/SetConsoleMode are safe FFI calls;
    // the handle comes from GetStdHandle and *mode is a plain u32 we own.
    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE).unwrap_or_else(|e| {
            eprintln!("error: failed to get stdin handle: {e}");
            std::process::exit(1);
        });
        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(handle, &mut mode).is_err() {
            eprintln!(
                "error: failed to get console mode (is stdin a console?)"
            );
            std::process::exit(1);
        }

        // Without ENABLE_LINE_INPUT each keypress is delivered immediately
        // rather than being buffered until Enter; without ENABLE_ECHO_INPUT
        // the bytes are not printed back (the harness records them).
        mode.0 &= !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT).0;
        if SetConsoleMode(handle, mode).is_err() {
            eprintln!("error: failed to set console mode");
            std::process::exit(1);
        }
    }
}
