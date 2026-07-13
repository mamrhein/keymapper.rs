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
        eprintln!("  check   Validate and diagnose the configuration");
        process::exit(1);
    }

    match args[2].as_str() {
        "list" => cmd_config_list(),
        "check" => cmd_config_check(),
        other => {
            eprintln!("Unknown subcommand: {other}");
            eprintln!();
            eprintln!("Available subcommands: list, check");
            process::exit(1);
        }
    }
}

fn load_config() -> (std::path::PathBuf, String) {
    let Some(path) = keymapperd::config_path::find_config_path() else {
        keymapperd::config_path::print_search_locations();
        process::exit(1);
    };

    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("Failed to read {}: {err}", path.display());
            process::exit(1);
        }
    };

    (path, contents)
}

fn cmd_config_list() {
    let (_path, contents) = load_config();
    print!("{contents}");
}

fn cmd_config_check() {
    let (path, contents) = load_config();

    let config = match keymapperd::config::AppConfig::load_from_str(&contents)
    {
        Ok(c) => c,
        Err(err) => {
            eprintln!("Failed to parse {}: {err}", path.display());
            process::exit(1);
        }
    };

    let diagnostics = config.check();

    if diagnostics.is_empty() {
        println!("{}: no issues found.", path.display());
    } else {
        println!("{}:", path.display());
        for (i, msg) in diagnostics.iter().enumerate() {
            println!("  {} {}", i + 1, msg);
        }
    }
}
