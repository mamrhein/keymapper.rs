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
use keymapper::{
    cli::{daemon_cmd, keyboard_cmd, keys_cmd},
    common::{
        app_identity,
        config::{AppConfig, KeyEvent, RuleGroup},
        keyboard::KeyboardSpecifier,
    },
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
    Status {
        /// Directory containing the PID file (`keymapperd.pid`).
        ///
        /// When specified, checks the PID-file (development) backend.  When
        /// omitted, checks the platform service manager.
        #[arg(long)]
        config_dir: Option<PathBuf>,
    },

    /// Start keymapperd if it is not already running.
    Start {
        /// Directory containing `config.yaml`.
        ///
        /// When specified, spawns keymapperd as a background process with
        /// this directory as its working directory.  The PID is
        /// written to `<path>/keymapperd.pid` for later stop/restart.
        ///
        /// When omitted, uses the platform service manager (launchd /
        /// systemd) to manage the daemon.  This is the production
        /// mode.
        #[arg(long)]
        config_dir: Option<PathBuf>,
    },

    /// Stop keymapperd if it is running.
    Stop {
        /// Directory containing the PID file (`keymapperd.pid`).
        ///
        /// When specified, stops the process that was started with
        /// `--config-dir`.  When omitted, uses the platform service manager.
        #[arg(long)]
        config_dir: Option<PathBuf>,
    },

    /// Restart keymapperd (stop then start).
    Restart {
        /// Directory containing `config.yaml` and the PID file.
        ///
        /// When specified, uses PID-based process management.  When omitted,
        /// uses the platform service manager.
        #[arg(long)]
        config_dir: Option<PathBuf>,
    },
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
    List,

    /// Validate and diagnose the configuration.
    Check {
        /// Path to a config file or directory containing `config.yaml`.
        ///
        /// When omitted, the standard search locations are used (the
        /// platform-specific application config directory; the current
        /// working directory is also searched in e2e builds).
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
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Appnames => cmd_appnames()?,
        Commands::Config { command } => match command {
            ConfigCommands::List => cmd_config_list()?,
            ConfigCommands::Check { path } => cmd_config_check(path)?,
            ConfigCommands::Create { dir } => cmd_config_create(dir)?,
            ConfigCommands::Add {
                trigger,
                output,
                group,
                apps,
                keyboard,
                keyboards_global,
            } => cmd_config_add(
                &trigger,
                &output,
                &group,
                apps,
                keyboard,
                keyboards_global,
            )?,
        },
        Commands::Keys { command } => match command {
            KeysCommands::List => cmd_keys_list()?,
            KeysCommands::Probe => cmd_keys_probe(),
        },
        Commands::Keyboards => cmd_keyboards(),
        Commands::Daemon { command } => match command {
            DaemonCommands::Status { config_dir } => {
                cmd_daemon_status(config_dir)?
            }
            DaemonCommands::Start { config_dir } => {
                cmd_daemon_start(config_dir)?
            }
            DaemonCommands::Stop { config_dir } => {
                cmd_daemon_stop(config_dir)?
            }
            DaemonCommands::Restart { config_dir } => {
                cmd_daemon_restart(config_dir)?
            }
        },
    }

    Ok(())
}

fn load_config() -> Result<(PathBuf, String), Box<dyn std::error::Error>> {
    let path = keymapper::common::config_path::find_config_path_strict()
        .map_err(|e| -> Box<dyn std::error::Error> {
            eprintln!("Error: {e}");
            std::process::exit(1);
        })?;

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
    let names = app_identity::list_app_names();

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

    let config =
        keymapper::common::config::AppConfig::load_from_str(&contents)
            .map_err(|err| {
                format!("failed to parse {}: {err}", path.display())
            })?;

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

fn cmd_config_create(
    dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = match dir {
        Some(d) => d.join("config.yaml"),
        None => keymapper::common::config_path::default_config_path()
            .ok_or("could not determine default config directory")?,
    };

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

/// Parse a keyboard specifier string into a `KeyboardSpecifier`.
///
/// The expected format is comma-separated key=value pairs, e.g.
/// `"name=Magic Keyboard,vendor=Apple"`.  Valid keys are `name`, `vendor`,
/// `model`, and `port`.
fn parse_keyboard_spec(s: &str) -> Result<KeyboardSpecifier, String> {
    let mut spec = KeyboardSpecifier {
        name: None,
        vendor: None,
        model: None,
        port: None,
    };

    if s.is_empty() {
        return Err("keyboard specifier is empty".to_string());
    }

    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        let (key, value) = part.split_once('=').ok_or_else(|| {
            format!(
                "invalid keyboard specifier part '{}': expected key=value \
                 format (valid keys: name, vendor, model, port)",
                part
            )
        })?;

        let key = key.trim();
        let value = value.trim().to_string();

        match key {
            "name" => spec.name = Some(value),
            "vendor" => spec.vendor = Some(value),
            "model" => spec.model = Some(value),
            "port" => spec.port = Some(value),
            _ => {
                return Err(format!(
                    "unknown keyboard specifier field '{}': valid keys are \
                     name, vendor, model, port",
                    key
                ));
            }
        }
    }

    if spec.is_empty() {
        return Err("keyboard specifier must have at least one field (name, \
                    vendor, model, or port)"
            .to_string());
    }

    Ok(spec)
}

/// Parse a list of keyboard specifier strings into `Vec<KeyboardSpecifier>`.
fn parse_keyboard_specs(
    args: Option<Vec<String>>,
) -> Result<Option<Vec<KeyboardSpecifier>>, String> {
    match args {
        Some(args) if !args.is_empty() => {
            let specs = args
                .into_iter()
                .map(|s| parse_keyboard_spec(&s))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(specs))
        }
        Some(_) | None => Ok(None),
    }
}

fn cmd_config_add(
    trigger_str: &str,
    output_str: &str,
    group_name: &str,
    apps: Option<Vec<String>>,
    keyboard_args: Option<Vec<String>>,
    keyboards_global_args: Option<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Parse the trigger and output.
    let trigger = KeyEvent::parse(trigger_str)
        .map_err(|e| format!("invalid trigger '{}': {e}", trigger_str))?;
    let output = KeyEvent::parse(output_str)
        .map_err(|e| format!("invalid output '{}': {e}", output_str))?;

    // Parse keyboard specifiers.
    let group_keyboards = parse_keyboard_specs(keyboard_args)
        .map_err(|e| format!("invalid --keyboard: {e}"))?;
    let global_keyboards = parse_keyboard_specs(keyboards_global_args)
        .map_err(|e| format!("invalid --keyboards-global: {e}"))?;

    // Find an existing config file.
    let path = keymapper::common::config_path::find_config_path().ok_or_else(
        || {
            eprintln!(
                "No configuration file found. Create one with `keymapper \
                 config create`"
            );
            "configuration file not found"
        },
    )?;

    // Load existing config.  `find_config_path` guarantees the file exists.
    reject_symlink(&path)?;
    let contents = fs_err::read_to_string(&path)?;
    let mut config = AppConfig::load_from_str(&contents)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))?;

    // Apply global keyboard filter if provided.
    if let Some(gk) = global_keyboards {
        config.keyboards = Some(gk);
    }

    // Find or create the target group.
    let mut group = config
        .groups
        .iter_mut()
        .find(|g| g.name.as_deref() == Some(group_name));

    if group.is_none() {
        config.groups.push(RuleGroup {
            name: Some(group_name.to_string()),
            apps: apps.clone().unwrap_or_default(),
            keyboards: group_keyboards.clone().unwrap_or_default(),
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

    // If --keyboard was given, apply it to the group (only if creating new or
    // the group has no keyboards yet).
    if let (Some(g), Some(kb)) = (&mut group, &group_keyboards)
        && g.keyboards.is_empty()
    {
        g.keyboards = kb.clone();
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

fn cmd_daemon_status(
    config_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let backend = daemon_cmd::Backend::from_config_dir(config_dir);

    if daemon_cmd::is_running(&backend) {
        println!("keymapperd is running");
    } else {
        println!("keymapperd is not running");
    }

    Ok(())
}

fn cmd_daemon_start(
    config_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let backend = daemon_cmd::Backend::from_config_dir(config_dir);

    if daemon_cmd::is_running(&backend) {
        println!("keymapperd is already running");
        return Ok(());
    }

    daemon_cmd::start(&backend)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    println!("keymapperd started");

    Ok(())
}

fn cmd_daemon_stop(
    config_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let backend = daemon_cmd::Backend::from_config_dir(config_dir);

    if !daemon_cmd::is_running(&backend) {
        println!("keymapperd is not running");
        return Ok(());
    }

    daemon_cmd::stop(&backend)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    println!("keymapperd stopped");

    Ok(())
}

fn cmd_daemon_restart(
    config_dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let backend = daemon_cmd::Backend::from_config_dir(config_dir);

    daemon_cmd::restart(&backend)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    println!("keymapperd restarted");

    Ok(())
}

fn cmd_keys_list() -> Result<(), Box<dyn std::error::Error>> {
    keys_cmd::list();
    Ok(())
}

fn cmd_keys_probe() {
    keys_cmd::probe();
}

fn cmd_keyboards() {
    keyboard_cmd::list();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keyboard_spec_name_only() {
        let spec = parse_keyboard_spec("name=Magic Keyboard").unwrap();
        assert_eq!(spec.name, Some("Magic Keyboard".to_string()));
        assert!(spec.vendor.is_none());
    }

    #[test]
    fn parse_keyboard_spec_multiple_fields() {
        let spec =
            parse_keyboard_spec("name=Magic Keyboard,vendor=Apple").unwrap();
        assert_eq!(spec.name, Some("Magic Keyboard".to_string()));
        assert_eq!(spec.vendor, Some("Apple".to_string()));
    }

    #[test]
    fn parse_keyboard_spec_all_fields() {
        let spec = parse_keyboard_spec(
            "name=Magic Keyboard,vendor=Apple,model=0x05ac,port=USB",
        )
        .unwrap();
        assert_eq!(spec.name, Some("Magic Keyboard".to_string()));
        assert_eq!(spec.vendor, Some("Apple".to_string()));
        assert_eq!(spec.model, Some("0x05ac".to_string()));
        assert_eq!(spec.port, Some("USB".to_string()));
    }

    #[test]
    fn parse_keyboard_spec_trims_whitespace() {
        let spec = parse_keyboard_spec("  name = Magic Keyboard ").unwrap();
        assert_eq!(spec.name, Some("Magic Keyboard".to_string()));
    }

    #[test]
    fn parse_keyboard_spec_empty_input() {
        let err = parse_keyboard_spec("").unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn parse_keyboard_spec_invalid_format() {
        let err = parse_keyboard_spec("nosign").unwrap_err();
        assert!(err.contains("key=value"));
    }

    #[test]
    fn parse_keyboard_spec_unknown_field() {
        let err = parse_keyboard_spec("foobar=hello").unwrap_err();
        assert!(err.contains("unknown"));
    }

    #[test]
    fn parse_keyboard_specs_none_input() {
        let result = parse_keyboard_specs(None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_keyboard_specs_empty_list() {
        let result = parse_keyboard_specs(Some(vec![])).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_keyboard_specs_valid_list() {
        let result = parse_keyboard_specs(Some(vec![
            "name=Keyboard1".to_string(),
            "vendor=Logitech".to_string(),
        ]))
        .unwrap();
        let specs = result.unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].name, Some("Keyboard1".to_string()));
        assert_eq!(specs[1].vendor, Some("Logitech".to_string()));
    }
}
