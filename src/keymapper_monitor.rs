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
/// Creates a small focused window, captures all keyboard events, and writes
/// them to an output file in the format `down <Key>` / `up <Key>`.
#[derive(Parser, Debug)]
#[command(
    name = "keymapper_monitor",
    version,
    about = "Cross-platform keyboard event monitor for e2e testing.",
    long_about = "Creates a small focused window, captures all keyboard \
                  events,\nand writes them to an output file in the format \
                  `down <Key>` / `up <Key>`."
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
