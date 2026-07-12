// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{collections::HashMap, fs, path::Path};

use crate::config::{AppConfig, KeyAction};

/// Platform-native keycode width.  Chosen so that the cache, the Lookup
/// trait, and the OS-level APIs all agree — eliminating runtime casts.
#[cfg(target_os = "macos")]
pub type NativeKey = u16; // CGKeyCode

#[cfg(target_os = "windows")]
pub type NativeKey = u16; // VIRTUAL_KEY (SendInput)

#[cfg(target_os = "linux")]
pub type NativeKey = u16; // evdev::Key::code()

/// Platform-native structural action layout.
#[derive(Debug, Clone)]
pub enum NativeAction {
    RemapTo(NativeKey),
    Shortcut(Vec<NativeKey>),
}

/// Compiled key-mapping cache optimised for fast runtime lookups.
///
/// Internal fields are private so that consumers go through the
/// [`crate::state::Lookup`] trait instead of reaching into the HashMaps.
pub struct RuntimeLookupCache {
    process_map: HashMap<String, HashMap<NativeKey, NativeAction>>,
    global_map: HashMap<NativeKey, NativeAction>,
}

// Re-expose the maps read-only via the Lookup trait impl in state.rs.
// Keep them accessible for the internal compilation step.
impl RuntimeLookupCache {
    pub(crate) fn process_map(
        &self,
    ) -> &HashMap<String, HashMap<NativeKey, NativeAction>> {
        &self.process_map
    }

    pub(crate) fn global_map(&self) -> &HashMap<NativeKey, NativeAction> {
        &self.global_map
    }
}

impl RuntimeLookupCache {
    /// Load a TOML config file, parse it, and compile the lookup cache
    /// in one step.  Used by both initialisation and hot-reload.
    pub fn compile_from_path<P: AsRef<Path>>(
        path: P,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(path)?;
        let parsed = AppConfig::load_from_str(&content)?;
        Ok(Self::compile_from_config(&parsed))
    }

    pub fn compile_from_config(app_config: &AppConfig) -> Self {
        let mut process_map: HashMap<
            String,
            HashMap<NativeKey, NativeAction>,
        > = HashMap::new();
        let mut global_map: HashMap<NativeKey, NativeAction> = HashMap::new();

        for rule in &app_config.rules {
            // Zero-cost cast: the Key discriminant IS the native code.
            let native_trigger = rule.trigger.as_native();

            let native_action = match &rule.action {
                KeyAction::RemapTo(target_key) => {
                    NativeAction::RemapTo(target_key.as_native())
                }
                KeyAction::Shortcut(target_keys) => {
                    let compiled_keys =
                        target_keys.iter().map(|k| k.as_native()).collect();
                    NativeAction::Shortcut(compiled_keys)
                }
            };

            if rule.applications.is_empty() {
                global_map.insert(native_trigger, native_action.clone());
            } else {
                for app in &rule.applications {
                    process_map
                        .entry(app.to_lowercase())
                        .or_default()
                        .insert(native_trigger, native_action.clone());
                }
            }
        }

        RuntimeLookupCache {
            process_map,
            global_map,
        }
    }
}
