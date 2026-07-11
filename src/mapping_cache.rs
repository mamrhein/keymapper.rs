// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::collections::HashMap;

use crate::{
    config::{AppConfig, KeyAction},
    os_bridge::abstract_to_native_code,
};

/// Platform-native keycode width.  Chosen so that the cache, the Lookup
/// trait, and the OS-level APIs all agree — eliminating runtime casts.
#[cfg(target_os = "macos")]
pub type NativeKey = u16; // CGKeyCode

#[cfg(target_os = "windows")]
pub type NativeKey = u16; // KEYBDINPUT.wVk (SendInput)

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
    pub fn compile_from_config(app_config: &AppConfig) -> Self {
        let mut process_map: HashMap<
            String,
            HashMap<NativeKey, NativeAction>,
        > = HashMap::new();
        let mut global_map: HashMap<NativeKey, NativeAction> = HashMap::new();

        for rule in &app_config.rules {
            let native_trigger = abstract_to_native_code(&rule.trigger);

            let native_action = match &rule.action {
                KeyAction::RemapTo(target_key) => {
                    NativeAction::RemapTo(abstract_to_native_code(target_key))
                }
                KeyAction::Shortcut(target_keys) => {
                    let compiled_keys = target_keys
                        .iter()
                        .map(abstract_to_native_code)
                        .collect();
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
