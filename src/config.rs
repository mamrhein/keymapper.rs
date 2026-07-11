// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use serde::{Deserialize, Serialize};

/// Global abstract key definitions used in the configuration file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AbstractKey {
    CapsLock,
    LeftControl,
    LeftAlt,
    Space,
    F1,
    F2,
    LetterA,
    LetterT,
}

/// The type of action to execute when a key condition matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeyAction {
    /// Remap to another single key (e.g., CapsLock -> LeftControl)
    RemapTo(AbstractKey),
    /// Simulate a multi-key macro shortcut (e.g., F1 -> Ctrl + T)
    Shortcut(Vec<AbstractKey>),
}

/// A specific mapping rule bound to applications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingRule {
    pub description: Option<String>,
    /// List of target applications (process names or bundle IDs)
    pub applications: Vec<String>,
    pub trigger: AbstractKey,
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
