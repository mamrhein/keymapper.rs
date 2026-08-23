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

use crate::common::{
    config::AppConfig, hid_usage::HidUsage, keyboard::KeyboardSpecifier,
};

// ---------------------------------------------------------------------------
// Modifier bitmask layout (u8): specific key bits only.
//
// bit 0: left control      bit 4: right control
// bit 1: left shift        bit 5: right shift
// bit 2: left alt          bit 6: right alt
// bit 3: left command/win  bit 7: right command/win
//
// The bit positions match the HID modifier usage ids (0xE0–0xE7), so left
// side modifiers occupy bits 0–3 and right side modifiers bits 4–7.
//
// Input matching uses exact equality.  "Either side" semantics (e.g. "ctrl"
// matching left or right) are achieved by compile-time rule expansion: a
// rule with "ctrl" produces two entries, one with bit 0 and one with bit 4.
// ---------------------------------------------------------------------------

/// A platform-native key event: modifiers held together with a base key press.
/// Used uniformly for both input matching and output emission.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NativeKey {
    /// Bitmask of specific modifier keys currently held.
    pub modifiers: u8,
    /// HID usage of the base key (any key, including modifier keys).
    pub usage: HidUsage,
}

/// A single compiled rule: trigger key paired with output events.
/// Multiple entries may share the same base key but differ in modifiers.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    /// HID usage of the trigger's base key.
    pub usage: HidUsage,
    /// Exact modifier bitmask for matching.
    pub modifiers: u8,
    /// Output events to emit when this rule matches.
    pub outputs: Vec<NativeKey>,
    /// Per-rule keyboard filter. `None` means the rule matches all
    /// keyboards; when set, only events from matching keyboards trigger
    /// this rule.
    pub keyboards: Option<Vec<KeyboardSpecifier>>,
}

/// Compiled key-mapping cache optimised for fast runtime lookups.
///
/// All rules store the trigger as a `HidUsage`, so lookups are keyed by the
/// full page-specific usage and ids that repeat across pages never collide.
/// Modifier discrimination happens at lookup time by scanning entries with
/// matching modifier bits.  The first match wins, preserving definition
/// order within each app scope.
#[derive(Debug)]
pub struct RuntimeLookupCache {
    /// Per-app rules: app name -> list of compiled rules.
    process_rules: IndexMap<String, Vec<CompiledRule>>,
    /// Global rules: list of compiled rules.
    global_rules: Vec<CompiledRule>,
    /// Global keyboard filter. `None` means all keyboards pass.
    global_keyboards: Option<Vec<KeyboardSpecifier>>,
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

    pub fn global_keyboards(&self) -> Option<&Vec<KeyboardSpecifier>> {
        self.global_keyboards.as_ref()
    }
}

impl RuntimeLookupCache {
    /// Load a YAML config file, parse it, and compile the lookup cache
    /// in one step.  Used by initialisation.
    pub fn compile_from_path<P: AsRef<Path>>(
        path: P,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(path)?;
        Self::compile_from_str(&content)
    }

    /// Compile a lookup cache from a YAML config string.  Used by hot-reload
    /// to accept content read from an already-open file handle.
    pub fn compile_from_str(
        content: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let parsed = AppConfig::load_from_str(content)?;
        Ok(Self::compile_from_config(&parsed))
    }

    pub fn compile_from_config(app_config: &AppConfig) -> Self {
        let mut process_rules: IndexMap<String, Vec<CompiledRule>> =
            IndexMap::new();
        let mut global_rules: Vec<CompiledRule> = Vec::new();

        // Extract the global keyboard filter.  Treat empty lists as no
        // filter to match the config semantics.
        let global_keyboards = app_config
            .keyboards
            .as_ref()
            .filter(|kbs| !kbs.is_empty())
            .cloned();

        // Iterate groups in definition order.  First-match-wins is
        // guaranteed by preserving insertion order.
        for group in &app_config.groups {
            if group.mappings.is_empty() {
                continue;
            }

            let apps: Vec<String> = group.apps.clone();

            // Capture the per-group keyboard filter.  An empty list means
            // no restriction, consistent with config semantics.
            let group_keyboards = if group.keyboards.is_empty() {
                None
            } else {
                Some(group.keyboards.clone())
            };

            for (trigger, outputs) in group.mappings.iter() {
                let native_outputs = compile_outputs(outputs);

                // Expand modifier variants for "either side" semantics.
                let variants = expand_modifier_bits(&trigger.modifiers);

                for mod_bits in variants {
                    let rule = CompiledRule {
                        usage: trigger.base,
                        modifiers: mod_bits,
                        outputs: native_outputs.clone(),
                        keyboards: group_keyboards.clone(),
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
            global_keyboards,
        }
    }
}

/// Compile a list of output key events into native form.
fn compile_outputs(
    events: &[crate::common::config::KeyEvent],
) -> Vec<NativeKey> {
    events
        .iter()
        .map(|event| NativeKey {
            modifiers: compile_modifier_bits(&event.modifiers),
            usage: event.base,
        })
        .collect()
}

/// Compile modifier keys into a specific bitmask.
///
/// Each modifier contributes its own specific bit (left vs right is
/// preserved).
fn compile_modifier_bits(modifiers: &[HidUsage]) -> u8 {
    let mut bits: u8 = 0;
    for usage in modifiers {
        if let Some(bit) = HidUsage::hid_usage_to_modifier_bit(*usage) {
            bits |= 1 << bit;
        }
    }
    bits
}

/// Expand modifier keys into all "either side" variant bitmasks.
///
/// For generic aliases (e.g. "Ctrl" -> LeftControl), both left and right
/// bits are valid matches.  For specific keys (e.g. "RightControl"), only
/// the corresponding bit matches.
///
/// Returns a list of bitmasks, one per variant.  A bare key (no modifiers)
/// produces a single entry: `vec![0]`.
fn expand_modifier_bits(modifiers: &[HidUsage]) -> Vec<u8> {
    if modifiers.is_empty() {
        return vec![0];
    }

    // Collect the possible bit positions for each modifier.  Left-side
    // modifier usages come from generic aliases ("Ctrl" -> LeftControl) and
    // match both left and right bits.  Right-side usages match only their
    // specific bit.  Left side modifiers occupy bits 0–3, so the matching
    // right side bit is always 4 positions higher.
    let choices: Vec<Vec<u8>> = modifiers
        .iter()
        .map(|usage| match HidUsage::hid_usage_to_modifier_bit(*usage) {
            Some(bit @ 0..=3) => vec![bit, bit + 4],
            Some(bit) => vec![bit],
            // Non-modifier in modifier position.
            None => vec![0],
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
    // bit 0: left control,   bit 4: right control
    // bit 1: left shift,     bit 5: right shift
    // bit 2: left alt,       bit 6: right alt
    // bit 3: left command,   bit 7: right command

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
        // LeftControl maps to modifier group [0, 4], so "either side" matches.
        let result = expand_modifier_bits(&[HidUsage::LeftControl]);
        assert_eq!(result, vec![1 << 0, 1 << 4]);
    }

    #[test]
    fn expand_single_specific_modifier() {
        // RightControl matches only its own bit (bit 4).
        let result = expand_modifier_bits(&[HidUsage::RightControl]);
        assert_eq!(result, vec![1 << 4]);
    }

    #[test]
    fn expand_two_modifiers() {
        // Ctrl + Shift → cartesian product of {0,4} × {1,5}.
        let result = expand_modifier_bits(&[
            HidUsage::LeftControl,
            HidUsage::LeftShift,
        ]);
        assert_eq!(
            result,
            vec![
                (1 << 0) | (1 << 1), // left ctrl + left shift
                (1 << 0) | (1 << 5), // left ctrl + right shift
                (1 << 4) | (1 << 1), // right ctrl + left shift
                (1 << 4) | (1 << 5), // right ctrl + right shift
            ]
        );
    }

    #[test]
    fn expand_three_modifiers() {
        // Ctrl + Shift + Alt → 2×2×2 = 8 variants.
        let result = expand_modifier_bits(&[
            HidUsage::LeftControl,
            HidUsage::LeftShift,
            HidUsage::LeftAlt,
        ]);
        assert_eq!(result.len(), 8);
    }

    #[test]
    fn expand_non_modifier_in_modifiers_list() {
        // Non-modifier usages have no modifier bit and fall back to vec![0].
        // In the cartesian product this means bit 0 is set — a quirk of the
        // fallback path.
        let result = expand_modifier_bits(&[HidUsage::A]);
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
        assert_eq!(compile_modifier_bits(&[HidUsage::LeftControl]), 1 << 0);
        assert_eq!(compile_modifier_bits(&[HidUsage::LeftShift]), 1 << 1);
        assert_eq!(compile_modifier_bits(&[HidUsage::LeftAlt]), 1 << 2);
        assert_eq!(compile_modifier_bits(&[HidUsage::LeftCommand]), 1 << 3);
        assert_eq!(compile_modifier_bits(&[HidUsage::RightControl]), 1 << 4);
        assert_eq!(compile_modifier_bits(&[HidUsage::RightShift]), 1 << 5);
        assert_eq!(compile_modifier_bits(&[HidUsage::RightAlt]), 1 << 6);
        assert_eq!(compile_modifier_bits(&[HidUsage::RightCommand]), 1 << 7);
    }

    #[test]
    fn compile_modifier_bits_multiple() {
        assert_eq!(
            compile_modifier_bits(&[
                HidUsage::LeftControl,
                HidUsage::LeftShift
            ]),
            (1 << 0) | (1 << 1)
        );
    }

    #[test]
    fn compile_modifier_bits_non_modifier_ignored() {
        // Non-modifiers don't contribute a bit.
        assert_eq!(compile_modifier_bits(&[HidUsage::A]), 0);
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
        assert_eq!(rule.usage, HidUsage::CapsLock);
        assert_eq!(rule.modifiers, 0);
        assert_eq!(rule.outputs.len(), 1);
        assert_eq!(rule.outputs[0].usage, HidUsage::LeftControl);
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

        let usages: Vec<HidUsage> =
            cache.global_rules().iter().map(|r| r.usage).collect();
        let mods: Vec<u8> =
            cache.global_rules().iter().map(|r| r.modifiers).collect();

        assert!(usages.contains(&HidUsage::H));
        assert_eq!(mods.len(), 2);
        assert!(mods.contains(&(1 << 0))); // left control
        assert!(mods.contains(&(1 << 4))); // right control
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
        assert_eq!(rule.outputs[0].usage, HidUsage::LeftArrow);
        // Cmd resolves to LeftCommand → bit 3.
        assert_eq!(rule.outputs[0].modifiers, 1 << 3);
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
        assert_eq!(rule.outputs[0].usage, HidUsage::T);
        assert_eq!(rule.outputs[0].modifiers, 1 << 3); // Cmd
        assert_eq!(rule.outputs[1].usage, HidUsage::F1);
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
        assert_eq!(rules[0].usage, HidUsage::A);
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

        assert_eq!(rule.usage, HidUsage::CapsLock);
        assert_eq!(rule.modifiers, 0); // bare key, no modifiers
        assert_eq!(rule.outputs.len(), 1);
        assert_eq!(rule.outputs[0].usage, HidUsage::L);
        assert_eq!(rule.outputs[0].modifiers, 1 << 2); // LeftAlt → bit 2
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
            assert_eq!(rule.usage, HidUsage::A);
            assert_eq!(rule.outputs[0].usage, HidUsage::F12);
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
            cache.global_rules()[0].outputs[0].usage,
            HidUsage::LeftControl
        );

        assert_eq!(
            cache.global_rules()[1].outputs[0].usage,
            HidUsage::RightControl
        );
    }

    // -----------------------------------------------------------------------
    // keyboard filter compilation
    // -----------------------------------------------------------------------

    #[test]
    fn compile_global_keyboard_filter() {
        let yaml = r#"
keyboards:
  - name: "Magic Keyboard"
groups:
  - mappings:
      CapsLock: LeftControl
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let cache = RuntimeLookupCache::compile_from_config(&config);

        // Global filter is stored.
        let kbs = cache.global_keyboards();
        assert!(kbs.is_some());
        let kbs = kbs.unwrap();
        assert_eq!(kbs.len(), 1);
        assert_eq!(kbs[0].name.as_deref(), Some("Magic Keyboard"));
    }

    #[test]
    fn compile_no_global_keyboard_filter_when_omitted() {
        let yaml = r#"
groups:
  - mappings:
      CapsLock: LeftControl
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let cache = RuntimeLookupCache::compile_from_config(&config);

        // No global filter.
        assert!(cache.global_keyboards().is_none());
    }

    #[test]
    fn compile_no_global_keyboard_filter_when_empty() {
        let yaml = r#"
keyboards: []
groups:
  - mappings:
      CapsLock: LeftControl
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let cache = RuntimeLookupCache::compile_from_config(&config);

        // Empty list is treated as no filter.
        assert!(cache.global_keyboards().is_none());
    }

    #[test]
    fn compile_per_group_keyboard_filter() {
        let yaml = r#"
groups:
  - name: "magic only"
    keyboards:
      - name: "Magic Keyboard"
        vendor: "Apple"
    mappings:
      CapsLock: LeftControl
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let cache = RuntimeLookupCache::compile_from_config(&config);

        // The compiled rule carries the group's keyboard filter.
        assert_eq!(cache.global_rules().len(), 1);
        let rule = &cache.global_rules()[0];
        assert!(rule.keyboards.is_some());
        let kbs = rule.keyboards.as_ref().unwrap();
        assert_eq!(kbs.len(), 1);
        assert_eq!(kbs[0].name.as_deref(), Some("Magic Keyboard"));
        assert_eq!(kbs[0].vendor.as_deref(), Some("Apple"));
    }

    #[test]
    fn compile_no_per_rule_filter_when_group_has_none() {
        let yaml = r#"
groups:
  - mappings:
      CapsLock: LeftControl
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let cache = RuntimeLookupCache::compile_from_config(&config);

        // No per-rule filter when the group has no keyboards.
        let rule = &cache.global_rules()[0];
        assert!(rule.keyboards.is_none());
    }

    #[test]
    fn compile_keyboard_filter_expands_with_modifiers() {
        // "Ctrl+A" expands to two rules; both inherit the same keyboard
        // filter from the group.
        let yaml = r#"
groups:
  - keyboards:
      - vendor: "Logitech"
    mappings:
      Ctrl+A: F1
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let cache = RuntimeLookupCache::compile_from_config(&config);

        assert_eq!(cache.global_rules().len(), 2);
        for rule in cache.global_rules() {
            assert!(rule.keyboards.is_some());
            let kbs = rule.keyboards.as_ref().unwrap();
            assert_eq!(kbs[0].vendor.as_deref(), Some("Logitech"));
        }
    }

    #[test]
    fn compile_app_scoped_rule_inherits_group_keyboards() {
        let yaml = r#"
groups:
  - name: "app rules"
    apps: [MyApp]
    keyboards:
      - name: "Magic Keyboard"
    mappings:
      Ctrl+H: LeftArrow
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let cache = RuntimeLookupCache::compile_from_config(&config);

        let rules = cache.process_rules("MyApp").expect("MyApp rules");
        assert!(!rules.is_empty());
        for rule in rules {
            assert!(rule.keyboards.is_some());
            let kbs = rule.keyboards.as_ref().unwrap();
            assert_eq!(kbs[0].name.as_deref(), Some("Magic Keyboard"));
        }
    }

    #[test]
    fn compile_global_and_per_group_filters_independently() {
        let yaml = r#"
keyboards:
  - vendor: "Apple"
groups:
  - name: "magic only"
    keyboards:
      - name: "Magic Keyboard"
    mappings:
      CapsLock: LeftControl
"#;
        let config = AppConfig::load_from_str(yaml).unwrap();
        let cache = RuntimeLookupCache::compile_from_config(&config);

        // Both global and per-rule filters are independently stored.
        let gk = cache.global_keyboards();
        assert!(gk.is_some());
        assert_eq!(gk.as_ref().unwrap()[0].vendor.as_deref(), Some("Apple"));

        let rk = cache.global_rules()[0].keyboards.as_ref();
        assert!(rk.is_some());
        assert_eq!(rk.unwrap()[0].name.as_deref(), Some("Magic Keyboard"));
    }
}
