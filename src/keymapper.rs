// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The `keymapper` CLI binary: argument parsing and command dispatch.
//!
//! All command bodies live in [`keymapper::cli`]; this file only declares the
//! clap surface and routes each parsed subcommand to its implementation.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use keymapper::{
    cli::{appnames_cmd, config_cmd, daemon_cmd, keyboard_cmd, keys_cmd},
    daemon::logging::LevelFilter,
};

/// CLI utility for managing the keymapperd configuration.
#[derive(Parser)]
#[command(name = "keymapper")]
#[command(version)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List applications that own visible windows.
    ///
    /// Prints the canonical app name — the exact value to use in the `apps`
    /// field of your config.yaml — followed by a human-readable display name
    /// where it differs.
    Appnames,

    /// Configuration file management.
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// List all connected keyboard devices.
    ///
    /// Shows the name, vendor, model, port type, and device identifier for
    /// each detected keyboard.  The device identifier can be used to
    /// filter key events for per-device mapping rules.
    Keyboards,

    /// Key introspection tools.
    Keys {
        #[command(subcommand)]
        command: KeysCommands,
    },

    /// Daemon process management.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },
}

#[derive(Subcommand)]
enum DaemonCommands {
    /// Check whether keymapperd is running.
    Status,

    /// Start keymapperd if it is not already running.
    Start,

    /// Stop keymapperd if it is running.
    Stop,

    /// Restart keymapperd (stop then start).
    Restart,

    /// Change the log level of a running keymapperd without a restart.
    ///
    /// Talks to the daemon over its control socket; the log output itself is
    /// unchanged (journal / Event Viewer / the stderr fallback).
    Log {
        /// The new log level.
        #[arg(long, value_parser = parse_log_level_arg)]
        level: LevelFilter,
    },
}

/// Parse a `--level` value into a [`LevelFilter`], with a readable clap error.
fn parse_log_level_arg(value: &str) -> Result<LevelFilter, String> {
    value.parse().map_err(|_| {
        "expected one of: error, warn, info, debug, or trace".to_string()
    })
}

#[derive(Subcommand)]
enum KeysCommands {
    /// Print all key names recognised in the configuration file.
    ///
    /// These are the canonical names grouped by category that can be used
    /// as triggers and outputs in key-mapping rules.
    List,

    /// Wait for physical key presses and print each key's name and code.
    ///
    /// Press Control+Escape to exit.
    Probe,
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Print the configuration file to stdout.
    List {
        /// Path to a config file or directory containing `config.yaml`.
        ///
        /// When omitted, the standard search locations are used (the
        /// platform-specific application config directory).
        path: Option<PathBuf>,
    },

    /// Validate and diagnose the configuration.
    Check {
        /// Path to a config file or directory containing `config.yaml`.
        ///
        /// When omitted, the standard search locations are used (the
        /// platform-specific application config directory).
        path: Option<PathBuf>,
    },

    /// Create an empty configuration file at the given directory or the
    /// default platform-specific location when omitted.
    Create {
        /// Directory where `config.yaml` will be created.
        ///
        /// When omitted, the file is placed in the default platform-specific
        /// application config directory (e.g. `~/Library/Application
        /// Support/keymapperd` on macOS).
        dir: Option<PathBuf>,
    },

    /// Add a key-mapping rule to the configuration.
    Add {
        /// Trigger key event (e.g. "CapsLock", "Ctrl+H").
        trigger: String,

        /// Output key event (e.g. "LeftControl", "Cmd+Shift+T").
        output: String,

        /// Group name. Creates the group if it doesn't exist.
        #[arg(short, long, default_value = "default")]
        group: String,

        /// Comma-separated app names to scope this rule.
        #[arg(short, long)]
        apps: Option<Vec<String>>,

        /// Keyboard specifier(s) for this group, as key=value pairs.
        ///
        /// Multiple specifiers can be passed by repeating the flag.  Within
        /// a single value, key=value pairs are separated by commas.
        /// E.g. `--keyboard "name=Magic Keyboard,vendor=Apple"`
        #[arg(long)]
        keyboard: Option<Vec<String>>,

        /// Set global keyboard filter(s).  Same syntax as `--keyboard`.
        ///
        /// When present, only events from matching keyboards are processed
        /// at all.
        #[arg(long)]
        keyboards_global: Option<Vec<String>>,

        /// Path to a config file or directory containing `config.yaml`.
        ///
        /// When omitted, the standard search locations are used (the
        /// platform-specific application config directory).
        path: Option<PathBuf>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Appnames => appnames_cmd::run(),
        Commands::Config { command } => match command {
            ConfigCommands::List { path } => config_cmd::list(path)?,
            ConfigCommands::Check { path } => config_cmd::check(path)?,
            ConfigCommands::Create { dir } => config_cmd::create(dir)?,
            ConfigCommands::Add {
                trigger,
                output,
                group,
                apps,
                keyboard,
                keyboards_global,
                path,
            } => config_cmd::add(
                &trigger,
                &output,
                &group,
                apps,
                keyboard,
                keyboards_global,
                path,
            )?,
        },
        Commands::Keys { command } => match command {
            KeysCommands::List => keys_cmd::list(),
            KeysCommands::Probe => keys_cmd::probe(),
        },
        Commands::Keyboards => keyboard_cmd::list(),
        Commands::Daemon { command } => match command {
            DaemonCommands::Status => daemon_cmd::status(),
            DaemonCommands::Start => daemon_cmd::start()?,
            DaemonCommands::Stop => daemon_cmd::stop()?,
            DaemonCommands::Restart => daemon_cmd::restart()?,
            DaemonCommands::Log { level } => daemon_cmd::log(level)?,
        },
    }

    Ok(())
}
