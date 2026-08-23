// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::path::PathBuf;

use clap::Parser;

/// Cross-platform keyboard event monitor for e2e testing.
///
/// On Linux, grabs the daemon's uinput output device and logs its raw key
/// events (no window, deterministic, headless-friendly).  On Windows, a
/// low-level keyboard hook captures the daemon's tagged output (no window,
/// no keyboard-focus dependency).  On other platforms, creates a small
/// focused window that captures keyboard events.  Events are written to an
/// output file in the format `down <Key>` / `up <Key>`.
#[derive(Parser, Debug)]
#[command(
    name = "keymapper_monitor",
    version,
    about = "Cross-platform keyboard event monitor for e2e testing.",
    long_about = "On Linux, grabs the daemon's uinput output device and logs \
                  its raw key\nevents. On Windows, a low-level hook captures \
                  the daemon's tagged\noutput. On other platforms, creates a \
                  small focused window that\ncaptures keyboard events. \
                  Events are written to an output file\nin the format `down \
                  <Key>` / `up <Key>`."
)]
struct Args {
    /// Path to the output file where captured events are written.
    #[arg(short, long)]
    output: PathBuf,
}

fn main() {
    let args = Args::parse();
    keymapper::util::monitor::app::run(args.output);
}
