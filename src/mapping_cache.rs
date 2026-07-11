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

/// Platform-native structural action layout
#[derive(Debug, Clone)]
pub enum NativeAction {
    RemapTo(u32),
    Shortcut(Vec<u32>),
}

/// The ultimate performance runtime lookup cache structure.
pub struct RuntimeLookupCache {
    pub process_map: HashMap<String, HashMap<u32, NativeAction>>,
    pub global_map: HashMap<u32, NativeAction>,
}

impl RuntimeLookupCache {
    pub fn compile_from_config(app_config: &AppConfig) -> Self {
        let mut process_map: HashMap<String, HashMap<u32, NativeAction>> =
            HashMap::new();
        let mut global_map: HashMap<u32, NativeAction> = HashMap::new();

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
