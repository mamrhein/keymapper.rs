// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use keymapperd::config::{AppConfig, KeyEvent, RuleGroup};

mod apps;
mod keys;
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
    /// List application names for all visible windows.
    ///
    /// The printed names are the exact values that keymapperd uses to match
    /// rules against running applications.  Use them in the `apps` field of
    /// your config.yaml.
    Appnames,

    /// Configuration file management.
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Key introspection tools.
    Keys {
        #[command(subcommand)]
        command: KeysCommands,
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
enum KeysCommands {
    /// Print all key names recognised in the configuration file.
    ///
    /// These are the canonical names (sorted alphabetically) that can be used
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
    List,

    /// Validate and diagnose the configuration.
    Check {
        /// Path to a config file or directory containing `config.yaml`.
        ///
        /// When omitted, the standard search locations are used (CWD, then
        /// the platform-specific application config directory).
        path: Option<PathBuf>,
    },

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
        Commands::Appnames => cmd_appnames()?,
        Commands::Config { command } => match command {
            ConfigCommands::List => cmd_config_list()?,
            ConfigCommands::Check { path } => cmd_config_check(path)?,
            ConfigCommands::Create => cmd_config_create()?,
            ConfigCommands::Add {
                trigger,
                output,
                group,
                apps,
            } => cmd_config_add(&trigger, &output, &group, apps)?,
        },
        Commands::Keys { command } => match command {
            KeysCommands::List => cmd_keys_list()?,
            KeysCommands::Probe => cmd_keys_probe(),
        },
        Commands::Server { command } => match command {
            ServerCommands::Status => cmd_server_status()?,
            ServerCommands::Start => cmd_server_start()?,
        },
    }

    Ok(())
}

fn load_config() -> Result<(PathBuf, String), Box<dyn std::error::Error>> {
    let path = keymapperd::config_path::find_config_path_strict().map_err(
        |e| -> Box<dyn std::error::Error> {
            eprintln!("Error: {e}");
            std::process::exit(1);
        },
    )?;

    let contents = fs_err::read_to_string(&path)?;

    Ok((path, contents))
}

/// Load a config file from an explicit user-supplied path.
///
/// If *target* points to a regular file, that file is used.  If it points to
/// a directory, `config.yaml` inside that directory is used.  Symbolic links
/// are rejected in both cases.
fn load_config_at(
    target: &Path,
) -> Result<(PathBuf, String), Box<dyn std::error::Error>> {
    let path = if target.is_file() {
        target.to_path_buf()
    } else if target.is_dir() {
        target.join("config.yaml")
    } else {
        return Err(format!(
            "path '{}' does not exist or is not a file/directory",
            target.display()
        )
        .into());
    };

    reject_symlink(&path)?;

    if !path.is_file() {
        return Err(
            format!("config file not found: {}", path.display()).into()
        );
    }

    let contents = fs_err::read_to_string(&path)?;

    Ok((path, contents))
}

/// Check that a config file is not a symbolic link and return it if valid.
fn reject_symlink(path: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(path)
        .ok()
        .is_some_and(|m| m.file_type().is_symlink())
    {
        Err(format!(
            "config file {} is a symbolic link and will not be followed",
            path.display(),
        ))
    } else {
        Ok(())
    }
}

fn cmd_appnames() -> Result<(), Box<dyn std::error::Error>> {
    let names = apps::list_app_names();

    if names.is_empty() {
        println!("No visible applications found.");
        return Ok(());
    }

    for name in &names {
        println!("{name}");
    }

    Ok(())
}

fn cmd_config_list() -> Result<(), Box<dyn std::error::Error>> {
    let (_path, contents) = load_config()?;
    print!("{contents}");
    Ok(())
}

fn cmd_config_check(
    target: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (path, contents) = match target {
        Some(t) => load_config_at(&t)?,
        None => load_config()?,
    };

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

    // Load existing config or start fresh.  Reject symlinks on load.
    let mut config = if path.is_file() {
        reject_symlink(&path)?;
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
    if let (Some(g), Some(apps)) = (&mut group, &apps)
        && g.apps.is_empty()
    {
        g.apps = apps.clone();
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

fn cmd_keys_list() -> Result<(), Box<dyn std::error::Error>> {
    keys::list();
    Ok(())
}

fn cmd_keys_probe() {
    keys::probe();
}
