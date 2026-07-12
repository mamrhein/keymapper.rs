// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use serde::{Deserialize, Deserializer, Serialize};

// Re-export the platform-specific Key type so that downstream modules
// (mapping_cache, state, hot_reload) import it from this module.
pub(crate) use crate::os::Key;

/// A key press, optionally combined with modifier keys.
///
/// Accepts compact `+`-separated strings in TOML:
/// - `"CapsLock"` — single key, no modifiers
/// - `"ctrl+a"` — chord: ctrl held while pressing a
/// - `"cmd+shift+t"` — chord: cmd+shift held while pressing t
#[derive(Debug, Clone)]
pub enum ChordTrigger {
    /// Single key with no modifier requirement (e.g. CapsLock alone).
    Key(Key),
    /// Base key combined with specific modifiers (e.g. Ctrl+A).
    Chord { base: Key, modifiers: Vec<Key> },
}

impl<'de> Deserialize<'de> for ChordTrigger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for ChordTrigger {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Key(key) => serializer.serialize_str(key.as_str()),
            Self::Chord { base, modifiers } => {
                let parts: Vec<String> = modifiers
                    .iter()
                    .map(|k| k.as_str().to_string())
                    .chain(std::iter::once(base.as_str().to_string()))
                    .collect();
                serializer.serialize_str(&parts.join("+"))
            }
        }
    }
}

impl ChordTrigger {
    /// Parse a `+`-separated string into a trigger.
    fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split('+').collect();

        if parts.is_empty() || (parts.len() == 1 && parts[0].trim().is_empty())
        {
            return Err("empty trigger string".to_string());
        }

        if parts.len() == 1 {
            // Single key: "CapsLock", "a", "f1"
            let key = parse_key(parts[0])?;
            Ok(Self::Key(key))
        } else {
            // Chord: "ctrl+a", "cmd+shift+t"
            // Last token is the base key; all preceding tokens are modifiers.
            let base = parse_key(parts[parts.len() - 1])?;
            let modifiers: Result<Vec<Key>, _> = parts[..parts.len() - 1]
                .iter()
                .map(|p| parse_key(p))
                .collect();
            Ok(Self::Chord {
                base,
                modifiers: modifiers?,
            })
        }
    }
}

/// Parse a single token from the config string into a `Key`.
fn parse_key(token: &str) -> Result<Key, String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err("empty key token in trigger".to_string());
    }

    let lower = trimmed.to_lowercase();
    let canonical = crate::key_names::resolve_alias(&lower).unwrap_or(&lower);

    Key::from_canonical(canonical)
        .ok_or_else(|| crate::key_names::unknown_key_error(trimmed))
}

/// The type of action to execute when a key condition matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeyAction {
    /// Remap to another single key (e.g., CapsLock -> LeftControl)
    RemapTo(Key),
    /// Simulate a multi-key macro shortcut (e.g., F1 -> Ctrl + T)
    Shortcut(Vec<Key>),
}

/// A specific mapping rule bound to applications.
#[derive(Debug, Clone)]
pub struct MappingRule {
    pub description: Option<String>,
    /// List of target applications (process names or bundle IDs).
    /// Empty list means the rule applies globally.
    pub applications: Vec<String>,
    pub trigger: ChordTrigger,
    pub action: KeyAction,
}

impl<'de> Deserialize<'de> for MappingRule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RuleRaw {
            description: Option<String>,
            applications: Vec<String>,
            trigger: String,
            action: KeyAction,
        }

        let raw = RuleRaw::deserialize(deserializer)?;
        let trigger = ChordTrigger::parse(&raw.trigger)
            .map_err(serde::de::Error::custom)?;

        Ok(Self {
            description: raw.description,
            applications: raw.applications,
            trigger,
            action: raw.action,
        })
    }
}

impl Serialize for MappingRule {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut state = serializer.serialize_struct("MappingRule", 4)?;
        state.serialize_field("description", &self.description)?;
        state.serialize_field("applications", &self.applications)?;
        state.serialize_field("trigger", &self.trigger)?;
        state.serialize_field("action", &self.action)?;
        state.end()
    }
}

/// The root configuration layout representing the file structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub rules: Vec<MappingRule>,
}

impl AppConfig {
    pub fn load_from_str(toml_str: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_str)
    }
}
