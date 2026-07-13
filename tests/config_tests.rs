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

use keymapperd::config::{AppConfig, Key};

// ---------------------------------------------------------------------------
// Valid configs
// ---------------------------------------------------------------------------

#[test]
fn parse_empty_config() {
    let config = AppConfig::load_from_str("groups: []").unwrap();
    assert!(config.groups.is_empty());
}

#[test]
fn parse_global_group() {
    let yaml = r#"
- mappings:
    CapsLock: LeftControl
"#;
    let config = AppConfig::load_from_str(yaml).unwrap();
    assert_eq!(config.groups.len(), 1);

    let group = &config.groups[0];
    assert!(group.apps.is_empty());

    let mut mappings = group.mappings.iter();
    let (trigger, outputs) = mappings.next().unwrap();
    assert!(trigger.modifiers.is_empty());
    assert!(matches!(trigger.base, Key::CapsLock));
    assert_eq!(outputs.len(), 1);
    assert!(outputs[0].modifiers.is_empty());
    assert!(matches!(outputs[0].base, Key::LeftControl));
    assert!(mappings.next().is_none());
}

#[test]
fn parse_app_scoped_group() {
    let yaml = r#"
- name: "iterm nav"
  apps: [iTerm2]
  mappings:
    Ctrl+H: LeftArrow
    Ctrl+L: RightArrow
"#;
    let config = AppConfig::load_from_str(yaml).unwrap();
    assert_eq!(config.groups.len(), 1);

    let group = &config.groups[0];
    assert_eq!(group.name.as_deref(), Some("iterm nav"));
    assert_eq!(group.apps, vec!["iTerm2".to_string()]);

    let mut mappings = group.mappings.iter();

    // Ctrl+H -> LeftArrow
    let (trigger, outputs) = mappings.next().unwrap();
    assert_eq!(trigger.modifiers.len(), 1);
    assert!(matches!(trigger.modifiers[0], Key::LeftControl));
    assert!(matches!(trigger.base, Key::H));
    assert_eq!(outputs.len(), 1);
    assert!(outputs[0].modifiers.is_empty());
    assert!(matches!(outputs[0].base, Key::LeftArrow));

    // Ctrl+L -> RightArrow
    let (trigger, outputs) = mappings.next().unwrap();
    assert_eq!(trigger.modifiers.len(), 1);
    assert!(matches!(trigger.base, Key::L));
    assert!(matches!(outputs[0].base, Key::RightArrow));
}

#[test]
fn parse_multi_output() {
    let yaml = r#"
- mappings:
    CapsLock: [LeftControl, CapsLock]
"#;
    let config = AppConfig::load_from_str(yaml).unwrap();
    let group = &config.groups[0];

    let mut mappings = group.mappings.iter();
    let (_trigger, outputs) = mappings.next().unwrap();
    assert_eq!(outputs.len(), 2);
}

#[test]
fn parse_chord_output() {
    // A chord output: Cmd+L is a single event (hold Cmd, press L).
    let yaml = r#"
- mappings:
    RightAlt: LeftAlt+L
"#;
    let config = AppConfig::load_from_str(yaml).unwrap();
    let group = &config.groups[0];

    let mut mappings = group.mappings.iter();
    let (trigger, outputs) = mappings.next().unwrap();

    // Trigger: bare RightAlt
    assert!(trigger.modifiers.is_empty());
    assert!(matches!(trigger.base, Key::RightAlt));

    // Output: single event -- hold LeftAlt, press L
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].modifiers.len(), 1);
    assert!(matches!(outputs[0].modifiers[0], Key::LeftAlt));
    assert!(matches!(outputs[0].base, Key::L));
}

#[test]
fn parse_multiple_groups() {
    let yaml = r#"
- mappings:
    CapsLock: LeftControl

- name: "iterm nav"
  apps: [iTerm2]
  mappings:
    Ctrl+H: LeftArrow
"#;
    let config = AppConfig::load_from_str(yaml).unwrap();
    assert_eq!(config.groups.len(), 2);
    assert!(config.groups[0].apps.is_empty());
    assert_eq!(config.groups[1].apps.len(), 1);
}

#[test]
fn parse_group_without_mappings() {
    let yaml = r#"
- name: "placeholder"
"#;
    let config = AppConfig::load_from_str(yaml).unwrap();
    assert_eq!(config.groups.len(), 1);
    assert!(config.groups[0].mappings.is_empty());
}

#[test]
fn parse_complex_config() {
    let yaml = r#"
- mappings:
    CapsLock: LeftControl
    LeftControl: [LeftControl, CapsLock]

- name: "iterm nav"
  apps: [iTerm2]
  mappings:
    Ctrl+H: LeftArrow
    Ctrl+J: DownArrow
    Ctrl+K: UpArrow
    Ctrl+L: RightArrow

- name: "global shortcuts"
  mappings:
    Ctrl+Shift+LeftArrow: Cmd+LeftArrow
    Ctrl+Shift+RightArrow: Cmd+RightArrow
"#;
    let config = AppConfig::load_from_str(yaml).unwrap();
    assert_eq!(config.groups.len(), 3);

    // Global group: capslock swap
    assert!(config.groups[0].apps.is_empty());
    assert_eq!(config.groups[0].mappings.iter().count(), 2);

    // iTerm group
    assert_eq!(config.groups[1].apps, vec!["iTerm2".to_string()]);
    assert_eq!(config.groups[1].mappings.iter().count(), 4);

    // Global shortcuts -- chord outputs
    assert!(config.groups[2].apps.is_empty());
    assert_eq!(config.groups[2].mappings.iter().count(), 2);

    for (_trigger, outputs) in config.groups[2].mappings.iter() {
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].modifiers.len(), 1);
    }
}

#[test]
fn parse_case_sensitive() {
    // Key names are case-sensitive. "LeftControl" works, "leftcontrol" does
    // not.
    let yaml = r#"
- mappings:
    LeftControl: CapsLock
"#;
    let config = AppConfig::load_from_str(yaml).unwrap();
    let group = &config.groups[0];
    let mut mappings = group.mappings.iter();
    let (trigger, outputs) = mappings.next().unwrap();
    assert!(matches!(trigger.base, Key::LeftControl));
    assert!(matches!(outputs[0].base, Key::CapsLock));
}

// ---------------------------------------------------------------------------
// Error cases
// ---------------------------------------------------------------------------

#[test]
fn error_unknown_key_in_trigger() {
    let yaml = r#"
- mappings:
    XyZ123: CapsLock
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
    let err = result.unwrap_err();
    let err_str = format!("{}", err);
    assert!(
        err_str.contains("XyZ123"),
        "error message should mention the unknown key"
    );
}

#[test]
fn error_unknown_key_in_output() {
    let yaml = r#"
- mappings:
    CapsLock: FooBarBaz
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_lowercase_key_rejected() {
    let yaml = r#"
- mappings:
    capslock: LeftControl
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_empty_key_event_string() {
    let yaml = r#"
- mappings:
    "": CapsLock
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_trailing_plus_in_trigger() {
    let yaml = r#"
- mappings:
    "Ctrl+": CapsLock
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_double_plus_in_trigger() {
    let yaml = r#"
- mappings:
    "Ctrl++H": CapsLock
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_output_value_not_string() {
    // An integer output is invalid.
    let yaml = r#"
- mappings:
    CapsLock: 42
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_output_list_contains_non_string() {
    let yaml = r#"
- mappings:
    CapsLock: [LeftControl, 42]
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_unknown_key_in_chord_modifier() {
    let yaml = r#"
- mappings:
    Ctrl+BadModifier+H: CapsLock
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_unknown_key_in_chord_output() {
    let yaml = r#"
- mappings:
    CapsLock: Ctrl+NoSuchKey
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_invalid_yaml_structure() {
    // A plain string is not a valid config.
    let yaml = "just a string";
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_bare_mapping_no_key() {
    // Empty string as a key token inside a chord.
    let yaml = r#"
- mappings:
    "Ctrl+H": ""
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_output_sequence_has_nested_list() {
    // Nested sequence is not valid -- each element must be a string.
    let yaml = r#"
- mappings:
    CapsLock: [[LeftControl], CapsLock]
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_apps_with_wrong_type() {
    // `apps` must be a list of strings.
    let yaml = r#"
- name: "test"
  apps: "iTerm2"
  mappings:
    CapsLock: LeftControl
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_mappings_with_wrong_type() {
    // `mappings` must be a mapping, not a scalar.
    let yaml = r#"
- name: "test"
  mappings: "not a map"
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_unknown_top_level_field() {
    // When using map form, only "groups" is accepted.
    let yaml = r#"
unknown_field:
  - mappings:
      CapsLock: LeftControl
"#;
    let result = AppConfig::load_from_str(yaml);
    assert!(result.is_err());
}

#[test]
fn error_case_sensitive_app_name() {
    // App names are case-sensitive. "iterm2" does not match "iTerm2".
    let yaml = r#"
- name: "test"
  apps: [iterm2]
  mappings:
    CapsLock: LeftControl
"#;
    let config = AppConfig::load_from_str(yaml).unwrap();
    assert_eq!(config.groups[0].apps, vec!["iterm2".to_string()]);
}
