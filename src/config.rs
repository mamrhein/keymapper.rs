// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, de};

// Re-export the platform-specific Key type so that downstream modules
// (mapping_cache, state, hot_reload) and tests import it from this
// module.
pub use crate::os::Key;

/// A key event: modifiers held together with a base key press.
///
/// Accepts compact `+`-separated strings in YAML:
/// - `"CapsLock"` -- bare key press (no modifiers held)
/// - `"Ctrl+A"` -- Ctrl held while pressing A
/// - `"Cmd+Shift+T"` -- Cmd + Shift held while pressing T
/// - `"RightAlt+L"` -- right option held while pressing L
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyEvent {
    /// Modifier keys held during the event (empty for bare key presses).
    pub modifiers: Vec<Key>,
    /// The base key that is pressed (may itself be a modifier key, e.g.
    /// CapsLock).
    pub base: Key,
}

impl<'de> Deserialize<'de> for KeyEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(de::Error::custom)
    }
}

impl Serialize for KeyEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let parts: Vec<String> = self
            .modifiers
            .iter()
            .map(|k| k.as_str().to_string())
            .chain(std::iter::once(self.base.as_str().to_string()))
            .collect();
        serializer.serialize_str(&parts.join("+"))
    }
}

impl KeyEvent {
    /// Parse a `+`-separated string into a key event.
    ///
    /// The last token is the base key; all preceding tokens are modifiers.
    /// A single token (e.g. `"CapsLock"`) is a bare key press with no
    /// modifiers held, even if the token itself names a modifier key.
    pub fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split('+').collect();

        if parts.is_empty() || (parts.len() == 1 && parts[0].trim().is_empty())
        {
            return Err("empty key event string".to_string());
        }

        if parts.len() == 1 {
            // Bare key: "CapsLock", "A", "F1" — no modifiers held.
            let base = parse_key(parts[0])?;
            Ok(Self {
                modifiers: Vec::new(),
                base,
            })
        } else {
            // Chord: "Ctrl+A", "Cmd+Shift+T"
            // Last token is the base key; preceding tokens are modifiers.
            let base = parse_key(parts[parts.len() - 1])?;
            let modifiers: Result<Vec<Key>, _> = parts[..parts.len() - 1]
                .iter()
                .map(|p| parse_key(p))
                .collect();
            Ok(Self {
                base,
                modifiers: modifiers?,
            })
        }
    }
}

/// Parse a single token from the config string into a `Key`.
///
/// Key names are matched case-sensitively.
fn parse_key(token: &str) -> Result<Key, String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err("empty key token in event string".to_string());
    }

    Key::try_from_str(trimmed)
        .ok_or_else(|| crate::key_names::unknown_key_error(trimmed))
}

// ---------------------------------------------------------------------------
// MappingTable -- ordered key-event -> events mapping
// ---------------------------------------------------------------------------

/// An ordered collection of mappings from a trigger event to output events.
///
/// Stored as an `IndexMap` to guarantee first-match-wins semantics when the
/// cache is compiled.  Deserialized from a YAML mapping where keys are event
/// strings and values are either a single event string or a list of event
/// strings.
#[derive(Debug, Clone)]
pub struct MappingTable(IndexMap<KeyEvent, Vec<KeyEvent>>);

impl Default for MappingTable {
    fn default() -> Self {
        Self(IndexMap::new())
    }
}

impl MappingTable {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate over (trigger, outputs) pairs in definition order.
    pub fn iter(&self) -> impl Iterator<Item = (&KeyEvent, &[KeyEvent])> {
        self.0.iter().map(|(k, v)| (k, v.as_slice()))
    }

    /// Insert a new mapping entry.
    pub fn insert(&mut self, trigger: KeyEvent, outputs: Vec<KeyEvent>) {
        self.0.insert(trigger, outputs);
    }
}

/// Custom visitor that deserializes a YAML mapping into an ordered
/// `MappingTable`, preserving document order.
struct MappingTableVisitor;

impl<'de> de::Visitor<'de> for MappingTableVisitor {
    type Value = MappingTable;

    fn expecting(
        &self,
        formatter: &mut std::fmt::Formatter,
    ) -> std::fmt::Result {
        formatter.write_str("a mapping from event strings to event(s)")
    }

    fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
    where
        M: de::MapAccess<'de>,
    {
        let mut map = IndexMap::new();

        while let Some((key_str, value)) =
            access.next_entry::<String, serde_yaml::Value>()?
        {
            let trigger =
                KeyEvent::parse(&key_str).map_err(de::Error::custom)?;

            let outputs = match value {
                serde_yaml::Value::String(s) => {
                    let event =
                        KeyEvent::parse(&s).map_err(de::Error::custom)?;
                    vec![event]
                }
                serde_yaml::Value::Sequence(seq) => seq
                    .into_iter()
                    .map(|v| match v {
                        serde_yaml::Value::String(s) => {
                            KeyEvent::parse(&s).map_err(de::Error::custom)
                        }
                        _ => Err(de::Error::custom(
                            "expected an event string in output sequence",
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                _ => {
                    return Err(de::Error::custom(
                        "expected an event string or list of event strings",
                    ));
                }
            };

            map.insert(trigger, outputs);
        }

        Ok(MappingTable(map))
    }
}

impl<'de> Deserialize<'de> for MappingTable {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(MappingTableVisitor)
    }
}

impl Serialize for MappingTable {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (trigger, outputs) in &self.0 {
            let key = trigger_to_string(trigger);

            if outputs.len() == 1 {
                map.serialize_entry(&key, &outputs[0])?;
            } else {
                map.serialize_entry(&key, outputs.as_slice())?;
            }
        }
        map.end()
    }
}

/// Serialize a KeyEvent back to its `+`-separated string form.
fn trigger_to_string(event: &KeyEvent) -> String {
    event
        .modifiers
        .iter()
        .map(|k| k.as_str().to_string())
        .chain(std::iter::once(event.base.as_str().to_string()))
        .collect::<Vec<_>>()
        .join("+")
}

// ---------------------------------------------------------------------------
// RuleGroup -- app-scoped collection of mappings
// ---------------------------------------------------------------------------

/// A named group of key mappings, optionally scoped to specific applications.
///
/// When `apps` is empty the group applies globally (all applications).
/// Groups are evaluated in definition order; the first group whose app
/// scope matches is used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleGroup {
    /// Human-readable name for documentation/debugging.  Not required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Target applications (process names or bundle IDs).  An empty list
    /// means the group applies globally.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<String>,

    /// Ordered event-to-events mappings.  The first matching rule wins.
    #[serde(default, skip_serializing_if = "MappingTable::is_empty")]
    pub mappings: MappingTable,
}

// ---------------------------------------------------------------------------
// AppConfig -- root configuration document
// ---------------------------------------------------------------------------

/// The root configuration layout representing the YAML file structure.
///
/// The document is a sequence of rule groups.  Groups are evaluated in
/// definition order; within each group, mappings are evaluated in
/// definition order.  The first matching rule wins.
#[derive(Debug, Clone, Default)]
pub struct AppConfig {
    pub groups: Vec<RuleGroup>,
}

/// Deserializes AppConfig from either:
/// - A bare YAML sequence of rule groups (the primary format)
/// - A YAML mapping with a "groups" key (for programmatic use)
struct AppConfigVisitor;

impl<'de> de::Visitor<'de> for AppConfigVisitor {
    type Value = AppConfig;

    fn expecting(
        &self,
        formatter: &mut std::fmt::Formatter,
    ) -> std::fmt::Result {
        formatter.write_str(
            "a sequence of rule groups or a mapping with a 'groups' key",
        )
    }

    fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let mut groups = Vec::new();
        while let Some(group) = access.next_element::<RuleGroup>()? {
            groups.push(group);
        }
        Ok(AppConfig { groups })
    }

    fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
    where
        M: de::MapAccess<'de>,
    {
        let mut groups = Vec::<RuleGroup>::new();
        while let Some(key) = access.next_key::<String>()? {
            if key == "groups" {
                groups = access.next_value()?;
            } else {
                return Err(de::Error::unknown_field(&key, &["groups"]));
            }
        }
        Ok(AppConfig { groups })
    }
}

impl<'de> Deserialize<'de> for AppConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(AppConfigVisitor)
    }
}

impl Serialize for AppConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.groups.serialize(serializer)
    }
}

impl AppConfig {
    pub fn load_from_str(yaml_str: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml_str)
    }

    /// Analyse the parsed configuration and return a list of diagnostic
    /// findings.  Each finding is a human-readable message.
    pub fn check(&self) -> Vec<String> {
        let mut diagnostics = Vec::new();

        if self.groups.is_empty() {
            diagnostics.push("no rule groups defined".to_string());
        }

        // Collect all (trigger_key, group_index, group_name) entries to
        // detect duplicates.
        let mut seen_triggers: indexmap::IndexMap<
            KeyEvent,
            Vec<(usize, String)>,
        > = indexmap::IndexMap::new();

        for (group_idx, group) in self.groups.iter().enumerate() {
            let label = group
                .name
                .as_deref()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("group {}", group_idx + 1));

            if group.mappings.is_empty() {
                diagnostics.push(format!("'{}' has no mappings", label));
            }

            for (trigger, outputs) in group.mappings.iter() {
                seen_triggers
                    .entry(trigger.clone())
                    .or_default()
                    .push((group_idx, label.clone()));

                // No-op: the only output is identical to the trigger.
                if outputs.len() == 1 && outputs[0] == *trigger {
                    diagnostics.push(format!(
                        "'{}': {} remaps to itself (no-op)",
                        label,
                        trigger_to_string(trigger)
                    ));
                }
            }
        }

        // Duplicate triggers across groups.
        for (trigger, locations) in &seen_triggers {
            if locations.len() > 1 {
                let names: Vec<&str> =
                    locations.iter().map(|(_, name)| name.as_str()).collect();
                diagnostics.push(format!(
                    "trigger {} appears in multiple groups: {}",
                    trigger_to_string(trigger),
                    names.join(", ")
                ));
            }
        }

        // Circular pairs: A→B and B→A (both single-output rules).
        // Check per-group to avoid mappings in other groups overwriting
        // entries that form a circular pair within this group.
        let mut reported_pairs: Vec<(KeyEvent, KeyEvent)> = Vec::new();
        for group in &self.groups {
            let mut single_map: indexmap::IndexMap<KeyEvent, KeyEvent> =
                indexmap::IndexMap::new();
            for (trigger, outputs) in group.mappings.iter() {
                if outputs.len() == 1 {
                    single_map.insert(trigger.clone(), outputs[0].clone());
                }
            }

            for (trigger, output) in &single_map {
                // Self-maps are no-ops, not swaps.
                if trigger == output {
                    continue;
                }
                if let Some(back) = single_map.get(output)
                    && back == trigger
                {
                    // Avoid reporting both A→B and B→A.
                    let pair = if trigger < output {
                        (trigger.clone(), output.clone())
                    } else {
                        (output.clone(), trigger.clone())
                    };
                    if !reported_pairs.contains(&pair) {
                        reported_pairs.push(pair.clone());
                        diagnostics.push(format!(
                            "{} and {} form a circular pair (swap)",
                            trigger_to_string(&pair.0),
                            trigger_to_string(&pair.1)
                        ));
                    }
                }
            }
        }

        diagnostics
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Valid configs
    // -----------------------------------------------------------------------

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
        // Key names are case-sensitive. "LeftControl" works, "leftcontrol"
        // does not.
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

    // -----------------------------------------------------------------------
    // Error cases
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // check() diagnostics
    // -----------------------------------------------------------------------

    #[test]
    fn check_clean_returns_empty() {
        let yaml = r#"
- mappings:
    CapsLock: LeftControl
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        assert!(config.check().is_empty());
    }

    #[test]
    fn check_detects_no_op() {
        let yaml = r#"
- mappings:
    CapsLock: CapsLock
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let issues = config.check();
        assert!(issues.iter().any(|i| i.contains("remaps to itself")));
    }

    #[test]
    fn check_detects_duplicate_trigger() {
        let yaml = r#"
- mappings:
    CapsLock: LeftControl

- mappings:
    CapsLock: Tab
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let issues = config.check();
        assert!(
            issues
                .iter()
                .any(|i| i.contains("appears in multiple groups"))
        );
    }

    #[test]
    fn check_detects_empty_group() {
        let yaml = r#"
- name: "placeholder"
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let issues = config.check();
        assert!(issues.iter().any(|i| i.contains("has no mappings")));
    }

    #[test]
    fn check_detects_circular_swap() {
        let yaml = r#"
- mappings:
    CapsLock: LeftControl
    LeftControl: CapsLock
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let issues = config.check();
        assert!(issues.iter().any(|i| i.contains("circular pair")));
    }

    #[test]
    fn check_no_self_map_not_reported_as_swap() {
        // A self-map (no-op) should not be reported as a circular pair.
        let yaml = r#"
- mappings:
    A: A
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let issues = config.check();
        assert!(!issues.iter().any(|i| i.contains("circular pair")));
        assert!(issues.iter().any(|i| i.contains("remaps to itself")));
    }

    #[test]
    fn check_detects_empty_config() {
        let config = AppConfig::load_from_str("groups: []").unwrap();
        let issues = config.check();
        assert!(issues.iter().any(|i| i.contains("no rule groups")));
    }

    #[test]
    fn check_aggregates_multiple_issues() {
        let yaml = r#"
- name: "empty"

- mappings:
    A: A
    CapsLock: LeftControl
    LeftControl: CapsLock

- mappings:
    CapsLock: Tab
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let issues = config.check();

        assert!(issues.iter().any(|i| i.contains("has no mappings")));
        assert!(issues.iter().any(|i| i.contains("remaps to itself")));
        assert!(
            issues
                .iter()
                .any(|i| i.contains("appears in multiple groups"))
        );
        assert!(issues.iter().any(|i| i.contains("circular pair")));
    }
}
