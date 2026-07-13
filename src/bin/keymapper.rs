// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use keymapperd::config::{AppConfig, KeyEvent, RuleGroup};

mod server;

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
    /// Configuration file management.
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Daemon process management.
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },
}

#[derive(Subcommand)]
enum ServerCommands {
    /// Check whether keymapperd is running.
    Status,

    /// Start keymapperd if it is not already running.
    Start,
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Print the configuration file to stdout.
    List,

    /// Validate and diagnose the configuration.
    Check,

    /// Create an empty configuration file at the default location.
    Create,

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
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Config { command } => match command {
            ConfigCommands::List => cmd_config_list()?,
            ConfigCommands::Check => cmd_config_check()?,
            ConfigCommands::Create => cmd_config_create()?,
            ConfigCommands::Add {
                trigger,
                output,
                group,
                apps,
            } => cmd_config_add(&trigger, &output, &group, apps)?,
        },
        Commands::Server { command } => match command {
            ServerCommands::Status => cmd_server_status()?,
            ServerCommands::Start => cmd_server_start()?,
        },
    }

    Ok(())
}

fn load_config() -> Result<(PathBuf, String), Box<dyn std::error::Error>> {
    let path =
        keymapperd::config_path::find_config_path().ok_or_else(|| {
            keymapperd::config_path::print_search_locations();
            "configuration file not found".to_string()
        })?;

    let contents = fs_err::read_to_string(&path)?;

    Ok((path, contents))
}

fn cmd_config_list() -> Result<(), Box<dyn std::error::Error>> {
    let (_path, contents) = load_config()?;
    print!("{contents}");
    Ok(())
}

fn cmd_config_check() -> Result<(), Box<dyn std::error::Error>> {
    let (path, contents) = load_config()?;

    let config = keymapperd::config::AppConfig::load_from_str(&contents)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))?;

    let diagnostics = config.check();

    if diagnostics.is_empty() {
        println!("{}: no issues found.", path.display());
    } else {
        println!("{}:", path.display());
        for (i, msg) in diagnostics.iter().enumerate() {
            println!("  {} {}", i + 1, msg);
        }
    }

    Ok(())
}

fn cmd_config_create() -> Result<(), Box<dyn std::error::Error>> {
    let path = keymapperd::config_path::default_config_path()
        .ok_or("could not determine default config directory")?;

    // Check if the file already exists.
    if path.is_file() {
        return Err(format!(
            "configuration file already exists: {}",
            path.display()
        )
        .into());
    }

    // Create parent directory if needed.
    if let Some(parent) = path.parent() {
        fs_err::create_dir_all(parent)?;
    }

    // Write an empty config.
    let config = AppConfig::default();
    let yaml = serde_yaml::to_string(&config)?;
    fs_err::write(&path, &yaml)?;

    println!("Created empty configuration at {}", path.display());

    Ok(())
}

fn cmd_config_add(
    trigger_str: &str,
    output_str: &str,
    group_name: &str,
    apps: Option<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Parse the trigger and output.
    let trigger = KeyEvent::parse(trigger_str)
        .map_err(|e| format!("invalid trigger '{}': {e}", trigger_str))?;
    let output = KeyEvent::parse(output_str)
        .map_err(|e| format!("invalid output '{}': {e}", output_str))?;

    // Find or create the config file.  Prefer CWD for development
    // convenience, falling back to the platform default directory.
    let path = keymapperd::config_path::find_config_path()
        .or_else(|| {
            // No config exists yet — create one in CWD.
            let cwd_path = std::path::PathBuf::from("config.yaml");
            Some(cwd_path)
        })
        .or_else(keymapperd::config_path::default_config_path);

    let path = path.ok_or("could not determine config file location")?;

    // Load existing config or start fresh.
    let mut config = if path.is_file() {
        let contents = fs_err::read_to_string(&path)?;
        AppConfig::load_from_str(&contents).map_err(|err| {
            format!("failed to parse {}: {err}", path.display())
        })?
    } else {
        AppConfig::default()
    };

    // Find or create the target group.
    let mut group = config
        .groups
        .iter_mut()
        .find(|g| g.name.as_deref() == Some(group_name));

    if group.is_none() {
        config.groups.push(RuleGroup {
            name: Some(group_name.to_string()),
            apps: apps.clone().unwrap_or_default(),
            mappings: Default::default(),
        });
        group = Some(config.groups.last_mut().unwrap());
    }

    // If --apps was given, apply it to the group (only if creating new or
    // the group has no apps yet).
    if let (Some(g), Some(apps)) = (&mut group, &apps) {
        if g.apps.is_empty() {
            g.apps = apps.clone();
        }
    }

    // Add the mapping.
    if let Some(g) = group {
        g.mappings.insert(trigger, vec![output]);
    }

    // Write back.
    let yaml = serde_yaml::to_string(&config)?;
    fs_err::write(&path, &yaml)?;

    println!(
        "Added '{}' -> '{}' to group '{}'",
        trigger_str, output_str, group_name
    );

    Ok(())
}

fn cmd_server_status() -> Result<(), Box<dyn std::error::Error>> {
    if server::is_running() {
        println!("keymapperd is running");
    } else {
        println!("keymapperd is not running");
    }

    Ok(())
}

fn cmd_server_start() -> Result<(), Box<dyn std::error::Error>> {
    if server::is_running() {
        println!("keymapperd is already running");
        return Ok(());
    }

    server::start().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    println!("keymapperd started");

    Ok(())
}
