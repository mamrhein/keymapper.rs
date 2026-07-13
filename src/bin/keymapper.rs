// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::process;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 || args[1] != "config" {
        eprintln!("Usage: keymapper config <subcommand>");
        eprintln!();
        eprintln!("Subcommands:");
        eprintln!("  list    Print the configuration file to stdout");
        process::exit(1);
    }

    match args[2].as_str() {
        "list" => cmd_config_list(),
        other => {
            eprintln!("Unknown subcommand: {other}");
            eprintln!();
            eprintln!("Available subcommands: list");
            process::exit(1);
        }
    }
}

fn cmd_config_list() {
    let Some(path) = keymapperd::config_path::find_config_path() else {
        keymapperd::config_path::print_search_locations();
        process::exit(1);
    };

    match std::fs::read_to_string(&path) {
        Ok(contents) => print!("{contents}"),
        Err(err) => {
            eprintln!("Failed to read {}: {err}", path.display());
            process::exit(1);
        }
    }
}
