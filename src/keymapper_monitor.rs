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
use keymapper::test_util::monitor::writer::EventWriter;

/// Cross-platform keyboard event monitor for e2e testing.
///
/// On Linux, grabs the daemon's uinput output device and logs its raw key
/// events (no window, deterministic, headless-friendly).  On Windows, a
/// low-level keyboard hook captures every key reaching the session's hook
/// chain (no window, no keyboard-focus dependency).  On macOS, seizes the
/// daemon's Karabiner DriverKit virtual keyboard (no window, no
/// keyboard-focus dependency).  Events are written to an output file or to
/// stdout in the format `down <Key>` / `up <Key>`.
#[derive(Parser, Debug)]
#[command(
    name = "keymapper_monitor",
    version,
    about = "Cross-platform keyboard event monitor for e2e testing.",
    long_about = "On Linux, grabs the daemon's uinput output device and logs \
                  its raw key\nevents. On Windows, a low-level hook captures \
                  every key reaching\nthe session's hook chain. On macOS, \
                  seizes the daemon's\nKarabiner virtual keyboard. Events \
                  are written to an output file\nor to stdout in the format \
                  `down <Key>` / `up <Key>`."
)]
struct Args {
    /// Path to the output file where captured events are written.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Write captured events to stdout instead of a file.
    #[arg(short = 's', long)]
    stdout: bool,
}

fn main() {
    let args = Args::parse();

    let sink = match (&args.output, args.stdout) {
        (Some(path), false) => EventWriter::file(path).unwrap_or_else(|e| {
            eprintln!("error: failed to open output file: {e}");
            std::process::exit(1);
        }),
        (None, true) => EventWriter::stdout(),
        _ => {
            eprintln!("error: specify exactly one of --output or --stdout");
            std::process::exit(2);
        }
    };

    keymapper::test_util::monitor::run(sink);
}
