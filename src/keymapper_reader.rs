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
use keymapper::test_util::reader;

/// A "normal" stdin reader for e2e testing.
///
/// Puts its stdin in raw mode and records the characters the operating system
/// delivers to the focused terminal, appending them to the output file.  This
/// is the "what a real app receives" layer: mapped outputs and forwarded
/// passthroughs alike, at character fidelity.
#[derive(Parser, Debug)]
#[command(
    name = "keymapper_reader",
    version,
    about = "A normal stdin reader for e2e testing.",
    long_about = "Puts its stdin in raw mode and records the characters the \
                  operating system delivers\nto the focused terminal, \
                  appending them to the output file.\nThis is the \"what a \
                  real app receives\" layer: mapped outputs and\nforwarded \
                  passthroughs alike, at character fidelity."
)]
struct Args {
    /// Path to the file that receives the recorded bytes.
    output: PathBuf,
}

fn main() {
    let args = Args::parse();
    reader::run(&args.output);
}
