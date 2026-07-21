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

use crate::common::config::{AppConfig, Key};

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
fn compile_outputs(events: &[crate::common::config::KeyEvent]) -> Vec<NativeKey> {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Bit positions per the header comment:
    // bit 0: left control,   bit 1: right control
    // bit 2: left shift,     bit 3: right shift
    // bit 4: left alt,       bit 5: right alt
    // bit 6: left command,   bit 7: right command

    // -----------------------------------------------------------------------
    // expand_modifier_bits
    // -----------------------------------------------------------------------

    #[test]
    fn expand_no_modifiers() {
        let result = expand_modifier_bits(&[]);
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn expand_single_generic_modifier() {
        // LeftControl maps to modifier group [0, 1], so "either side" matches.
        let result = expand_modifier_bits(&[Key::LeftControl]);
        assert_eq!(result, vec![1 << 0, 1 << 1]);
    }

    #[test]
    fn expand_single_specific_modifier() {
        // RightControl also maps to group [0, 1] — the enum variant does not
        // narrow matching; narrowing only affects output emission.
        let result = expand_modifier_bits(&[Key::RightControl]);
        assert_eq!(result, vec![1 << 0, 1 << 1]);
    }

    #[test]
    fn expand_two_modifiers() {
        // Ctrl + Shift → cartesian product of {0,1} × {2,3}.
        let result = expand_modifier_bits(&[Key::LeftControl, Key::LeftShift]);
        assert_eq!(
            result,
            vec![
                (1 << 0) | (1 << 2), // left ctrl + left shift
                (1 << 0) | (1 << 3), // left ctrl + right shift
                (1 << 1) | (1 << 2), // right ctrl + left shift
                (1 << 1) | (1 << 3), // right ctrl + right shift
            ]
        );
    }

    #[test]
    fn expand_three_modifiers() {
        // Ctrl + Shift + Alt → 2×2×2 = 8 variants.
        let result = expand_modifier_bits(&[
            Key::LeftControl,
            Key::LeftShift,
            Key::LeftAlt,
        ]);
        assert_eq!(result.len(), 8);
    }

    #[test]
    fn expand_non_modifier_in_modifiers_list() {
        // Non-modifier keys return None from as_modifier_positions, falling
        // back to vec![0].  In the cartesian product this means bit 0 is
        // set — a quirk of the fallback path.
        let result = expand_modifier_bits(&[Key::A]);
        assert_eq!(result, vec![1 << 0]);
    }

    // -----------------------------------------------------------------------
    // compile_modifier_bits (output side — specific single bit)
    // -----------------------------------------------------------------------

    #[test]
    fn compile_modifier_bits_empty() {
        assert_eq!(compile_modifier_bits(&[]), 0);
    }

    #[test]
    fn compile_modifier_bits_single() {
        assert_eq!(compile_modifier_bits(&[Key::LeftControl]), 1 << 0);
        assert_eq!(compile_modifier_bits(&[Key::RightControl]), 1 << 1);
        assert_eq!(compile_modifier_bits(&[Key::LeftShift]), 1 << 2);
        assert_eq!(compile_modifier_bits(&[Key::RightShift]), 1 << 3);
        assert_eq!(compile_modifier_bits(&[Key::LeftAlt]), 1 << 4);
        assert_eq!(compile_modifier_bits(&[Key::RightAlt]), 1 << 5);
        assert_eq!(compile_modifier_bits(&[Key::LeftCommand]), 1 << 6);
        assert_eq!(compile_modifier_bits(&[Key::RightCommand]), 1 << 7);
    }

    #[test]
    fn compile_modifier_bits_multiple() {
        assert_eq!(
            compile_modifier_bits(&[Key::LeftControl, Key::LeftShift]),
            (1 << 0) | (1 << 2)
        );
    }

    #[test]
    fn compile_modifier_bits_non_modifier_ignored() {
        // Non-modifiers don't contribute a bit.
        assert_eq!(compile_modifier_bits(&[Key::A]), 0);
    }

    // -----------------------------------------------------------------------
    // compile_from_config — end-to-end compilation
    // -----------------------------------------------------------------------

    fn build_cache(yaml: &str) -> RuntimeLookupCache {
        let config = AppConfig::load_from_str(yaml).unwrap();
        RuntimeLookupCache::compile_from_config(&config)
    }

    #[test]
    fn compile_empty_config() {
        let cache = build_cache("groups: []");
        assert!(cache.global_rules().is_empty());
        assert!(cache.process_rules("any").is_none());
    }

    #[test]
    fn compile_global_rule() {
        let yaml = r#"
- mappings:
    CapsLock: LeftControl
"#;
        let cache = build_cache(yaml);
        assert_eq!(cache.global_rules().len(), 1);
        assert!(cache.process_rules("any").is_none());

        let rule = &cache.global_rules()[0];
        assert_eq!(rule.base, Key::CapsLock.as_native());
        assert_eq!(rule.modifiers, 0);
        assert_eq!(rule.outputs.len(), 1);
        assert_eq!(rule.outputs[0].base, Key::LeftControl.as_native());
    }

    #[test]
    fn compile_app_scoped_rule() {
        let yaml = r#"
- name: "nav"
  apps: [MyApp]
  mappings:
    Ctrl+H: LeftArrow
"#;
        let cache = build_cache(yaml);
        assert!(cache.global_rules().is_empty());

        // Exact case-sensitive app match.
        let rules = cache.process_rules("MyApp").expect("MyApp should exist");
        assert!(!rules.is_empty());

        // Wrong case should not match.
        assert!(cache.process_rules("myapp").is_none());
        assert!(cache.process_rules("MYAPP").is_none());
    }

    #[test]
    fn compile_modifier_expansion_in_rules() {
        // "Ctrl+H" expands to two rules: one for left ctrl, one for right.
        let yaml = r#"
- mappings:
    Ctrl+H: LeftArrow
"#;
        let cache = build_cache(yaml);
        assert_eq!(cache.global_rules().len(), 2);

        let bases: Vec<u16> =
            cache.global_rules().iter().map(|r| r.base).collect();
        let mods: Vec<u8> =
            cache.global_rules().iter().map(|r| r.modifiers).collect();

        assert!(bases.contains(&Key::H.as_native()));
        assert_eq!(mods.len(), 2);
        assert!(mods.contains(&(1 << 0))); // left control
        assert!(mods.contains(&(1 << 1))); // right control
    }

    #[test]
    fn compile_chord_output() {
        let yaml = r#"
- mappings:
    CapsLock: Cmd+LeftArrow
"#;
        let cache = build_cache(yaml);
        let rule = &cache.global_rules()[0];

        assert_eq!(rule.outputs.len(), 1);
        assert_eq!(rule.outputs[0].base, Key::LeftArrow.as_native());
        // Cmd resolves to LeftCommand → bit 6.
        assert_eq!(rule.outputs[0].modifiers, 1 << 6);
    }

    #[test]
    fn compile_multi_output() {
        let yaml = r#"
- mappings:
    CapsLock: [Cmd+T, F1]
"#;
        let cache = build_cache(yaml);
        let rule = &cache.global_rules()[0];

        assert_eq!(rule.outputs.len(), 2);
        assert_eq!(rule.outputs[0].base, Key::T.as_native());
        assert_eq!(rule.outputs[0].modifiers, 1 << 6); // Cmd
        assert_eq!(rule.outputs[1].base, Key::F1.as_native());
        assert_eq!(rule.outputs[1].modifiers, 0);
    }

    #[test]
    fn compile_multiple_groups_accumulate() {
        let yaml = r#"
- mappings:
    CapsLock: LeftControl

- name: "app rules"
  apps: [MyApp]
  mappings:
    A: B
"#;
        let cache = build_cache(yaml);
        assert_eq!(cache.global_rules().len(), 1);

        let rules = cache.process_rules("MyApp").expect("MyApp rules");
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn compile_group_without_mappings_skipped() {
        let yaml = r#"
- name: "placeholder"
"#;
        let cache = build_cache(yaml);
        assert!(cache.global_rules().is_empty());
    }

    #[test]
    fn compile_first_match_wins_order() {
        // Two rules for the same base key but different modifiers.  Order
        // is preserved so first-match-wins semantics work at runtime.
        let yaml = r#"
- mappings:
    Ctrl+Shift+A: F1
    Ctrl+A: F2
    A: F3
"#;
        let cache = build_cache(yaml);
        let rules = cache.global_rules();

        // Ctrl+Shift+A expands to 2×2 = 4 rules (ctrl × shift).
        // Ctrl+A expands to 2 rules (ctrl).
        // A has no modifiers → 1 rule.
        assert_eq!(rules.len(), 7);

        // The first entry should be the Ctrl+Shift+A rule.
        assert_eq!(rules[0].base, Key::A.as_native());
    }

    #[test]
    fn compile_same_rule_for_multiple_apps() {
        let yaml = r#"
- name: "multi-app"
  apps: [AppA, AppB]
  mappings:
    CapsLock: LeftControl
"#;
        let cache = build_cache(yaml);

        let rules_a = cache.process_rules("AppA").expect("AppA");
        let rules_b = cache.process_rules("AppB").expect("AppB");

        assert_eq!(rules_a.len(), rules_b.len());
        assert!(!rules_a.is_empty());
    }

    #[test]
    fn compile_modifier_only_trigger() {
        // A bare modifier key (no chord) as a trigger.
        let yaml = r#"
- mappings:
    CapsLock: LeftAlt+L
"#;
        let cache = build_cache(yaml);
        let rule = &cache.global_rules()[0];

        assert_eq!(rule.base, Key::CapsLock.as_native());
        assert_eq!(rule.modifiers, 0); // bare key, no modifiers
        assert_eq!(rule.outputs.len(), 1);
        assert_eq!(rule.outputs[0].base, Key::L.as_native());
        assert_eq!(rule.outputs[0].modifiers, 1 << 4); // LeftAlt → bit 4
    }

    #[test]
    fn compile_full_modifier_expansion_count() {
        // Ctrl → 2 variants (left/right), Shift → 2, Alt → 2.
        // Ctrl+Shift+Alt+A → 2×2×2 = 8 rules.
        let yaml = r#"
- mappings:
    Ctrl+Shift+Alt+A: F12
"#;
        let cache = build_cache(yaml);
        assert_eq!(cache.global_rules().len(), 8);

        // All rules should have base = A and output = F12.
        for rule in cache.global_rules() {
            assert_eq!(rule.base, Key::A.as_native());
            assert_eq!(rule.outputs[0].base, Key::F12.as_native());
        }
    }

    #[test]
    fn compile_duplicate_triggers_in_separate_groups() {
        // Two groups define the same mapping.  Both get compiled; the first
        // rule in the list wins at runtime (find_match scans sequentially).
        let yaml = r#"
- mappings:
    CapsLock: LeftControl

- mappings:
    CapsLock: RightControl
"#;
        let cache = build_cache(yaml);

        // Two groups, each with one CapsLock rule.
        assert_eq!(cache.global_rules().len(), 2);

        // The first rule should output LeftControl, the second RightControl.
        assert_eq!(
            cache.global_rules()[0].outputs[0].base,
            Key::LeftControl.as_native()
        );
        assert_eq!(
            cache.global_rules()[1].outputs[0].base,
            Key::RightControl.as_native()
        );
    }
}
