// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{fs, path::Path};

use indexmap::IndexMap;

use crate::config::{AppConfig, Key};

// ---------------------------------------------------------------------------
// Modifier bitmask layout (u8): specific key bits only.
//
// bit 0: left control      bit 1: right control
// bit 2: left shift        bit 3: right shift
// bit 4: left alt          bit 5: right alt
// bit 6: left command/win  bit 7: right command/win
//
// Input matching uses exact equality.  "Either side" semantics (e.g. "ctrl"
// matching left or right) are achieved by compile-time rule expansion: a
// rule with "ctrl" produces two entries, one with bit 0 and one with bit 1.
// ---------------------------------------------------------------------------

/// A platform-native key event: modifiers held together with a base key press.
/// Used uniformly for both input matching and output emission.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NativeKey {
    /// Bitmask of specific modifier keys currently held.
    pub modifiers: u8,
    /// Native key code of the base key (any key, including modifier keys).
    pub base: u16,
}

/// A single compiled rule: trigger key paired with output events.
/// Multiple entries may share the same base key but differ in modifiers.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    /// Native key code of the trigger's base key.
    pub base: u16,
    /// Exact modifier bitmask for matching.
    pub modifiers: u8,
    /// Output events to emit when this rule matches.
    pub outputs: Vec<NativeKey>,
}

/// Compiled key-mapping cache optimised for fast runtime lookups.
///
/// All rules use a unified `IndexMap` keyed by the base key code.  Modifier
/// discrimination happens at lookup time by scanning entries with matching
/// modifier bits.  The first match wins, preserving definition order within
/// each app scope.
pub struct RuntimeLookupCache {
    /// Per-app rules: app name -> list of compiled rules.
    process_rules: IndexMap<String, Vec<CompiledRule>>,
    /// Global rules: list of compiled rules.
    global_rules: Vec<CompiledRule>,
}

impl RuntimeLookupCache {
    pub(crate) fn process_rules(
        &self,
        app: &str,
    ) -> Option<&Vec<CompiledRule>> {
        self.process_rules.get(app)
    }

    pub(crate) fn global_rules(&self) -> &Vec<CompiledRule> {
        &self.global_rules
    }
}

impl RuntimeLookupCache {
    /// Load a YAML config file, parse it, and compile the lookup cache
    /// in one step.  Used by both initialisation and hot-reload.
    pub fn compile_from_path<P: AsRef<Path>>(
        path: P,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(path)?;
        let parsed = AppConfig::load_from_str(&content)?;
        Ok(Self::compile_from_config(&parsed))
    }

    pub fn compile_from_config(app_config: &AppConfig) -> Self {
        let mut process_rules: IndexMap<String, Vec<CompiledRule>> =
            IndexMap::new();
        let mut global_rules: Vec<CompiledRule> = Vec::new();

        // Iterate groups in definition order.  First-match-wins is
        // guaranteed by preserving insertion order.
        for group in &app_config.groups {
            if group.mappings.is_empty() {
                continue;
            }

            let apps: Vec<String> = group.apps.clone();

            for (trigger, outputs) in group.mappings.iter() {
                let native_outputs = compile_outputs(outputs);

                // Expand modifier variants for "either side" semantics.
                let variants = expand_modifier_bits(&trigger.modifiers);

                let trigger_base = trigger.base.as_native();

                for mod_bits in variants {
                    let rule = CompiledRule {
                        base: trigger_base,
                        modifiers: mod_bits,
                        outputs: native_outputs.clone(),
                    };

                    if apps.is_empty() {
                        global_rules.push(rule);
                    } else {
                        for app in &apps {
                            let rules =
                                process_rules.entry(app.clone()).or_default();
                            rules.push(rule.clone());
                        }
                    }
                }
            }
        }

        RuntimeLookupCache {
            process_rules,
            global_rules,
        }
    }
}

/// Compile a list of output key events into native form.
fn compile_outputs(events: &[crate::config::KeyEvent]) -> Vec<NativeKey> {
    events
        .iter()
        .map(|event| NativeKey {
            modifiers: compile_modifier_bits(&event.modifiers),
            base: event.base.as_native(),
        })
        .collect()
}

/// Compile modifier keys into a specific bitmask.
///
/// Each modifier contributes its own specific bit (left vs right is
/// preserved).
fn compile_modifier_bits(keys: &[Key]) -> u8 {
    let mut bits: u8 = 0;
    for key in keys {
        if let Some(bit) = key.as_modifier_bit() {
            bits |= 1 << bit;
        }
    }
    bits
}

/// Expand modifier keys into all "either side" variant bitmasks.
///
/// For group aliases (e.g. "ctrl" -> LeftControl), both left and right bits
/// are valid matches.  For specific keys (e.g. "rightctrl" -> RightControl),
/// only the corresponding bit matches.
///
/// Returns a list of bitmasks, one per variant.  A bare key (no modifiers)
/// produces a single entry: `vec![0]`.
fn expand_modifier_bits(modifiers: &[Key]) -> Vec<u8> {
    if modifiers.is_empty() {
        return vec![0];
    }

    // Collect the possible bit positions for each modifier.
    let choices: Vec<Vec<u8>> = modifiers
        .iter()
        .map(|key| {
            if let Some(positions) = key.as_modifier_positions() {
                positions
            } else {
                // Non-modifier in modifier position — should not happen,
                // but treat as no contribution.
                vec![0]
            }
        })
        .collect();

    // Generate the Cartesian product of bit combinations.
    let mut results: Vec<u8> = vec![0];
    for choice in choices {
        let mut next: Vec<u8> = Vec::new();
        for &acc in &results {
            for &bit in &choice {
                next.push(acc | (1 << bit));
            }
        }
        results = next;
    }

    results
}
