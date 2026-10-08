// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! `keymapper config` subcommands: list, check, create, and add.
//!
//! The command bodies live here (rather than in the `keymapper` binary) so
//! that the non-obvious `config add` merge rules have unit-test reach without
//! spawning the binary.  The pure model edit — finding or creating a group,
//! seeding its apps and keyboards, and inserting the mapping — is isolated in
//! `apply_add`; everything around it is path resolution and file I/O.

use std::path::{Path, PathBuf};

use crate::common::{
    config::{AppConfig, KeyEvent, RuleGroup},
    config_io::{read_config_content, write_config_atomic},
    config_path::{
        default_config_path, find_config_path, find_config_path_strict,
    },
    keyboard::KeyboardSpecifier,
};

/// Load the config from the default platform-specific search locations.
///
/// Reads through the daemon's hardened reader so the CLI enforces the same
/// constraints as the daemon (size cap, symlink, ownership, and
/// world-writable checks).
fn load_config() -> Result<(PathBuf, String), Box<dyn std::error::Error>> {
    let path = find_config_path_strict().map_err(
        |e| -> Box<dyn std::error::Error> {
            eprintln!("Error: {e}");
            std::process::exit(1);
        },
    )?;

    let contents = read_config_content(&path)
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?;

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

    if !path.is_file() {
        return Err(
            format!("config file not found: {}", path.display()).into()
        );
    }

    // The hardened reader rejects a symlinked config file (via symlink
    // metadata and `O_NOFOLLOW`) and applies the same size, ownership, and
    // world-writable checks as the daemon.
    let contents = read_config_content(&path)
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?;

    Ok((path, contents))
}

/// Print the configuration file to stdout.
pub fn list(
    target: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (path, contents) = match target {
        Some(t) => load_config_at(&t)?,
        None => load_config()?,
    };
    println!("{}:", path.display());
    print!("{contents}");
    Ok(())
}

/// Validate and diagnose the configuration.
pub fn check(
    target: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (path, contents) = match target {
        Some(t) => load_config_at(&t)?,
        None => load_config()?,
    };

    let config = AppConfig::load_from_str(&contents)
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

/// Create an empty configuration file at the given directory or the default
/// platform-specific location when omitted.
pub fn create(dir: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let path = match dir {
        Some(d) => d.join("config.yaml"),
        None => default_config_path()
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

    // Security check: validate existing ancestor directories before
    // create_dir_all, which follows symlinks for intermediate components.
    // Non-existent directories are allowed (they will be created); only
    // existing untrusted ancestors (world-writable without sticky bit,
    // non-directory components) are rejected (SEC-19).
    if let Some(parent) = path.parent() {
        crate::platform::config_access::verify_parent_chain_for_create(parent)
            .map_err(|e| format!("unsafe config directory: {e}"))?;
        fs_err::create_dir_all(parent)?;
    }

    // Write an empty config atomically with mode 0600.
    let config = AppConfig::default();
    let yaml = serde_saphyr::to_string(&config)?;
    write_config_atomic(&path, &yaml)
        .map_err(|err| format!("failed to write {}: {err}", path.display()))?;

    println!("Created empty configuration at {}", path.display());

    Ok(())
}

/// Add a key-mapping rule to the configuration.
///
/// Resolves and parses all inputs, applies the model edit with `apply_add`,
/// and writes the result back.  The merge rules themselves are unit-tested
/// against `apply_add` directly; here we only cover the I/O and the
/// user-facing error strings.
#[allow(clippy::too_many_arguments)]
pub fn add(
    trigger_str: &str,
    output_str: &str,
    group_name: &str,
    apps: Option<Vec<String>>,
    keyboard_args: Option<Vec<String>>,
    keyboards_global_args: Option<Vec<String>>,
    target: Option<PathBuf>,
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

    // Find and load the existing config file.
    let (path, contents) = match target {
        Some(t) => load_config_at(&t)?,
        None => {
            let path = find_config_path().ok_or_else(|| {
                eprintln!(
                    "No configuration file found. Create one with `keymapper \
                     config create`"
                );
                "configuration file not found"
            })?;

            // `find_config_path` guarantees the file exists and is not a
            // symlink; the hardened reader re-checks that and enforces the
            // size, ownership, and world-writable checks.
            let contents = read_config_content(&path).map_err(|err| {
                format!("failed to read {}: {err}", path.display())
            })?;

            (path, contents)
        }
    };
    let mut config = AppConfig::load_from_str(&contents)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))?;

    apply_add(
        &mut config,
        trigger,
        output,
        group_name,
        apps.as_deref(),
        group_keyboards.as_deref(),
        global_keyboards,
    );

    // Write back atomically with mode 0600.
    let yaml = serde_saphyr::to_string(&config)?;
    write_config_atomic(&path, &yaml)
        .map_err(|err| format!("failed to write {}: {err}", path.display()))?;

    println!(
        "Added '{}' -> '{}' to group '{}'",
        trigger_str, output_str, group_name
    );

    Ok(())
}

/// Apply an `add` to the in-memory config model.
///
/// This is the pure core of [`add`]: it finds or creates the target group,
/// applies the optional global keyboard filter, seeds a freshly created (or
/// not-yet-scoped) group's apps and keyboards, and inserts the mapping.  No
/// I/O and no process exit, so the merge rules are unit-testable.
pub(crate) fn apply_add(
    config: &mut AppConfig,
    trigger: KeyEvent,
    output: KeyEvent,
    group_name: &str,
    apps: Option<&[String]>,
    group_keyboards: Option<&[KeyboardSpecifier]>,
    global_keyboards: Option<Vec<KeyboardSpecifier>>,
) {
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
            apps: apps.map(<[String]>::to_vec).unwrap_or_default(),
            keyboards: group_keyboards
                .map(<[KeyboardSpecifier]>::to_vec)
                .unwrap_or_default(),
            mappings: Default::default(),
        });
        group = Some(config.groups.last_mut().unwrap());
    }

    // If --apps was given, apply it to the group (only if creating new or
    // the group has no apps yet).
    if let (Some(g), Some(apps)) = (&mut group, &apps)
        && g.apps.is_empty()
    {
        g.apps = apps.to_vec();
    }

    // If --keyboard was given, apply it to the group (only if creating new or
    // the group has no keyboards yet).
    if let (Some(g), Some(kb)) = (&mut group, &group_keyboards)
        && g.keyboards.is_empty()
    {
        g.keyboards = kb.to_vec();
    }

    // Add the mapping.
    if let Some(g) = group {
        g.mappings.insert(trigger, vec![output]);
    }
}

/// Parse a keyboard specifier string into a `KeyboardSpecifier`.
///
/// The expected format is comma-separated key=value pairs, e.g.
/// `"name=Magic Keyboard,vendor=Apple"`.  Valid keys are `name`, `vendor`,
/// `model`, and `port`.
pub(crate) fn parse_keyboard_spec(
    s: &str,
) -> Result<KeyboardSpecifier, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- keyboard-spec parsing --------------------------------------------

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

    // --- `config add` model edits -----------------------------------------

    /// A one-pair trigger/output used across the `apply_add` tests.
    fn pair() -> (KeyEvent, KeyEvent) {
        (
            KeyEvent::parse("CapsLock").unwrap(),
            KeyEvent::parse("LeftControl").unwrap(),
        )
    }

    fn group_named<'a>(config: &'a AppConfig, name: &str) -> &'a RuleGroup {
        config
            .groups
            .iter()
            .find(|g| g.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no group named {name:?}"))
    }

    #[test]
    fn apply_add_creates_named_group_with_mapping() {
        let (trigger, output) = pair();
        let mut config = AppConfig::default();
        assert!(config.groups.is_empty());

        apply_add(
            &mut config,
            trigger.clone(),
            output.clone(),
            "default",
            None,
            None,
            None,
        );

        assert_eq!(config.groups.len(), 1);
        let group = group_named(&config, "default");
        assert_eq!(group.mappings.get(&trigger), Some(&vec![output]));
        assert!(group.apps.is_empty());
        assert!(group.keyboards.is_empty());
    }

    #[test]
    fn apply_add_seeds_new_group_with_apps_and_keyboards() {
        let (trigger, output) = pair();
        let mut config = AppConfig::default();
        let apps = ["firefox".to_string()];
        let keyboards = vec![KeyboardSpecifier {
            name: Some("Magic Keyboard".to_string()),
            vendor: None,
            model: None,
            port: None,
        }];

        apply_add(
            &mut config,
            trigger,
            output,
            "g",
            Some(&apps),
            Some(&keyboards),
            None,
        );

        let group = group_named(&config, "g");
        assert_eq!(group.apps, apps.to_vec());
        assert_eq!(group.keyboards, keyboards);
    }

    #[test]
    fn apply_add_does_not_overwrite_existing_group_scope() {
        // A pre-existing group that already has apps/keyboards must keep
        // them; --apps/--keyboard only seed a group that has none yet.
        let (trigger, output) = pair();
        let mut config = AppConfig::default();
        config.groups.push(RuleGroup {
            name: Some("default".to_string()),
            apps: vec!["existing".to_string()],
            keyboards: vec![KeyboardSpecifier {
                name: Some("Existing".to_string()),
                vendor: None,
                model: None,
                port: None,
            }],
            mappings: Default::default(),
        });

        apply_add(
            &mut config,
            trigger.clone(),
            output,
            "default",
            Some(&["ignored".to_string()]),
            Some(&[KeyboardSpecifier {
                name: Some("Ignored".to_string()),
                vendor: None,
                model: None,
                port: None,
            }]),
            None,
        );

        let group = group_named(&config, "default");
        assert_eq!(group.apps, vec!["existing".to_string()]);
        assert_eq!(group.keyboards[0].name.as_deref(), Some("Existing"));
        // The mapping is still added.
        assert!(group.mappings.contains_key(&trigger));
    }

    #[test]
    fn apply_add_reuses_existing_group_instead_of_creating_a_second() {
        let (trigger, output) = pair();
        let mut config = AppConfig::default();
        config.groups.push(RuleGroup {
            name: Some("default".to_string()),
            apps: Vec::new(),
            keyboards: Vec::new(),
            mappings: Default::default(),
        });

        apply_add(&mut config, trigger, output, "default", None, None, None);

        assert_eq!(
            config.groups.len(),
            1,
            "must not create a duplicate group"
        );
    }

    #[test]
    fn apply_add_applies_global_keyboard_filter() {
        let (trigger, output) = pair();
        let mut config = AppConfig::default();
        assert!(config.keyboards.is_none());
        let global = vec![KeyboardSpecifier {
            name: Some("Only".to_string()),
            vendor: None,
            model: None,
            port: None,
        }];

        apply_add(
            &mut config,
            trigger,
            output,
            "default",
            None,
            None,
            Some(global.clone()),
        );

        assert_eq!(config.keyboards, Some(global));
    }

    #[test]
    fn apply_add_overwrites_mapping_for_same_trigger() {
        let (trigger, _) = pair();
        let output2 = KeyEvent::parse("LeftShift").unwrap();
        let mut config = AppConfig::default();
        apply_add(
            &mut config,
            trigger.clone(),
            pair().1,
            "default",
            None,
            None,
            None,
        );

        apply_add(
            &mut config,
            trigger.clone(),
            output2.clone(),
            "default",
            None,
            None,
            None,
        );

        let group = group_named(&config, "default");
        assert_eq!(group.mappings.get(&trigger), Some(&vec![output2]));
    }
}
