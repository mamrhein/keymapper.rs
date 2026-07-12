// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use serde::{Deserialize, Serialize};

// Re-export the platform-specific Key type so that downstream modules
// (mapping_cache, state, hot_reload) import it from this module.
pub(crate) use crate::os::Key;

/// The type of action to execute when a key condition matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeyAction {
    /// Remap to another single key (e.g., CapsLock -> LeftControl)
    RemapTo(Key),
    /// Simulate a multi-key macro shortcut (e.g., F1 -> Ctrl + T)
    Shortcut(Vec<Key>),
}

/// A specific mapping rule bound to applications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingRule {
    pub description: Option<String>,
    /// List of target applications (process names or bundle IDs).
    /// Empty list means the rule applies globally.
    pub applications: Vec<String>,
    pub trigger: Key,
    pub action: KeyAction,
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
