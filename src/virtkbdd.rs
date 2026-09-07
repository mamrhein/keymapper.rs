// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The virtkbdd daemon binary.
//!
//! A thin entry point around [`keymapper::platform::start_virtkbd`].  The real
//! implementation is macOS-only (it owns the root-only DriverKit virtual-HID
//! socket); on other platforms the binary prints an error and exits.

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    keymapper::platform::start_virtkbd()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("virtkbdd is only available on macOS.");
    std::process::exit(1);
}
