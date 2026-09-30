// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::collections::HashMap;

use super::focus::FocusTracker;
use crate::{
    common::{
        hid_usage::HidUsage,
        keyboard::{KeyboardInfo, KeyboardSpecifier},
    },
    keymap_core::{
        lookup::{Lookup, MutableLookup, find_match},
        mapping_cache::{NativeKey, RuntimeLookupCache},
    },
};

/// Live runtime state shared between the config hot-reloader and the
/// platform-specific event tap.
///
/// The rule lookup responsibilities (compiled cache and keyboard filtering)
/// live directly on this struct; the focused-application concern is
/// delegated to [`FocusTracker`].
pub struct RuntimeState {
    lookup_cache: RuntimeLookupCache,
    /// Maps platform device identifiers to full keyboard metadata.  Populated
    /// at startup from the platform's keyboard discovery and used to resolve
    /// device IDs to [`KeyboardInfo`] for keyboard filtering.
    keyboard_registry: HashMap<String, KeyboardInfo>,
    /// The currently focused application, refreshed through the injectable
    /// platform source behind a short-TTL single-flight cache.
    focus: FocusTracker,
}

impl std::fmt::Debug for RuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeState")
            .field("lookup_cache", &self.lookup_cache)
            .field("keyboard_registry", &self.keyboard_registry)
            .field("focus", &self.focus)
            .finish()
    }
}

impl RuntimeState {
    pub fn new(
        cache: RuntimeLookupCache,
        keyboards: Vec<KeyboardInfo>,
        active_app_source: Box<dyn Fn() -> String + Send + Sync>,
    ) -> Self {
        Self {
            lookup_cache: cache,
            keyboard_registry: keyboards
                .into_iter()
                .map(|kb| (kb.device.clone(), kb))
                .collect(),
            focus: FocusTracker::new(active_app_source),
        }
    }

    /// Resolve a platform device identifier to its full keyboard metadata.
    fn resolve_keyboard(&self, device_id: &str) -> Option<&KeyboardInfo> {
        self.keyboard_registry.get(device_id)
    }

    /// Check the global keyboard filter against a device ID.
    ///
    /// Returns `true` if the device is allowed (global filter is unset, or
    /// the device matches at least one specifier).
    fn check_global_keyboard_filter(&self, device_id: Option<&str>) -> bool {
        let Some(filter) = self.lookup_cache.global_keyboards() else {
            return true;
        };
        let Some(id) = device_id else {
            // No device ID available — the platform cannot identify the
            // source keyboard.  When a global filter is set but we have no
            // device info, we allow the event through.  This means keyboard
            // filtering is effectively bypassed on platforms that don't
            // expose per-keyboard device IDs.
            return true;
        };
        let Some(kb_info) = self.resolve_keyboard(id) else {
            // Unknown device — allow through to avoid silently dropping keys.
            return true;
        };

        // At least one specifier must match.
        filter.iter().any(|spec| spec.matches(kb_info))
    }

    /// Check a per-rule keyboard filter against a device ID.
    ///
    /// Returns `true` if the rule is allowed (no filter set, or the device
    /// matches at least one specifier).
    fn check_rule_keyboard_filter(
        &self,
        filter: &Option<Vec<KeyboardSpecifier>>,
        device_id: Option<&str>,
    ) -> bool {
        let Some(filter) = filter else {
            return true;
        };
        let Some(id) = device_id else {
            // Same rationale as the global check.
            return true;
        };
        let Some(kb_info) = self.resolve_keyboard(id) else {
            return true;
        };

        filter.iter().any(|spec| spec.matches(kb_info))
    }
}

impl Lookup for RuntimeState {
    fn for_app(
        &self,
        app: &str,
        usage: HidUsage,
        modifiers: u8,
        keyboard_device_id: Option<&str>,
    ) -> Option<&[NativeKey]> {
        // Check the global filter first.
        if !self.check_global_keyboard_filter(keyboard_device_id) {
            return None;
        }

        if let Some(rules) = self.lookup_cache.process_rules(app) {
            find_match(rules, usage, modifiers, |rule_keyboards| {
                self.check_rule_keyboard_filter(
                    rule_keyboards,
                    keyboard_device_id,
                )
            })
        } else {
            None
        }
    }

    fn global(
        &self,
        usage: HidUsage,
        modifiers: u8,
        keyboard_device_id: Option<&str>,
    ) -> Option<&[NativeKey]> {
        // Check the global filter first.
        if !self.check_global_keyboard_filter(keyboard_device_id) {
            return None;
        }

        find_match(
            self.lookup_cache.global_rules(),
            usage,
            modifiers,
            |rule_keyboards| {
                self.check_rule_keyboard_filter(
                    rule_keyboards,
                    keyboard_device_id,
                )
            },
        )
    }

    fn for_active_app(
        &self,
        usage: HidUsage,
        modifiers: u8,
        keyboard_device_id: Option<&str>,
    ) -> Option<&[NativeKey]> {
        self.for_app(&self.focus.get(), usage, modifiers, keyboard_device_id)
    }
}

impl MutableLookup for RuntimeState {
    fn set_lookup_cache(&mut self, cache: RuntimeLookupCache) {
        self.lookup_cache = cache;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::config::AppConfig;

    fn build_keyboard(
        name: &str,
        vendor: &str,
        model: &str,
        device: &str,
        port: Option<&str>,
    ) -> KeyboardInfo {
        KeyboardInfo::new(
            name.to_string(),
            vendor.to_string(),
            model.to_string(),
            device.to_string(),
            port.map(str::to_string),
        )
    }

    fn build_state(yaml: &str, keyboards: Vec<KeyboardInfo>) -> RuntimeState {
        let config = AppConfig::load_from_str(yaml).unwrap();
        let cache = RuntimeLookupCache::compile_from_config(&config);
        // Fixed source: these unit tests exercise rule matching and keyboard
        // filtering, never the active-app query, so a constant keeps them
        // deterministic and free of platform round-trips.
        RuntimeState::new(
            cache,
            keyboards,
            Box::new(|| "test_app".to_string()),
        )
    }

    // -----------------------------------------------------------------------
    // Global keyboard filter
    // -----------------------------------------------------------------------

    #[test]
    fn global_lookup_passes_when_no_filter() {
        let yaml = r#"
groups:
  - mappings:
      CapsLock: LeftControl
"#;
        let state = build_state(yaml, vec![]);
        let result = state.global(HidUsage::CapsLock, 0, None);
        assert!(result.is_some());
    }

    #[test]
    fn global_lookup_passes_when_device_matches_filter() {
        let yaml = r#"
keyboards:
  - name: "Magic Keyboard"
groups:
  - mappings:
      CapsLock: LeftControl
"#;
        let keyboards = vec![build_keyboard(
            "Magic Keyboard",
            "Apple",
            "0x05ac",
            "/dev/input/event3",
            Some("USB"),
        )];
        let state = build_state(yaml, keyboards);

        // Matching device passes.
        let result =
            state.global(HidUsage::CapsLock, 0, Some("/dev/input/event3"));
        assert!(result.is_some());
    }

    #[test]
    fn global_lookup_blocks_when_device_mismatches_filter() {
        let yaml = r#"
keyboards:
  - name: "Magic Keyboard"
groups:
  - mappings:
      CapsLock: LeftControl
"#;
        let keyboards = vec![build_keyboard(
            "Logitech K845",
            "Logitech",
            "K845",
            "/dev/input/event5",
            Some("Bluetooth"),
        )];
        let state = build_state(yaml, keyboards);

        // Non-matching device is blocked.
        let result =
            state.global(HidUsage::CapsLock, 0, Some("/dev/input/event5"));
        assert!(result.is_none());
    }

    #[test]
    fn global_lookup_passes_when_device_id_is_none() {
        // When the platform cannot identify the keyboard, events pass
        // through even if a global filter is set.
        let yaml = r#"
keyboards:
  - name: "Magic Keyboard"
groups:
  - mappings:
      CapsLock: LeftControl
"#;
        let state = build_state(yaml, vec![]);
        let result = state.global(HidUsage::CapsLock, 0, None);
        assert!(result.is_some());
    }

    #[test]
    fn global_lookup_passes_for_unknown_device() {
        // An unknown device (not in the registry) passes through to avoid
        // silently dropping keys.
        let yaml = r#"
keyboards:
  - name: "Magic Keyboard"
groups:
  - mappings:
      CapsLock: LeftControl
"#;
        let keyboards = vec![build_keyboard(
            "Magic Keyboard",
            "Apple",
            "0x05ac",
            "/dev/input/event3",
            Some("USB"),
        )];
        let state = build_state(yaml, keyboards);

        // Device not in registry passes through.
        let result =
            state.global(HidUsage::CapsLock, 0, Some("/dev/input/event99"));
        assert!(result.is_some());
    }

    // -----------------------------------------------------------------------
    // Per-rule keyboard filter
    // -----------------------------------------------------------------------

    #[test]
    fn per_rule_filter_allows_matching_device() {
        let yaml = r#"
groups:
  - keyboards:
      - vendor: "Apple"
    mappings:
      CapsLock: LeftControl
"#;
        let keyboards = vec![build_keyboard(
            "Magic Keyboard",
            "Apple",
            "0x05ac",
            "/dev/input/event3",
            Some("USB"),
        )];
        let state = build_state(yaml, keyboards);

        let result =
            state.global(HidUsage::CapsLock, 0, Some("/dev/input/event3"));
        assert!(result.is_some());
    }

    #[test]
    fn per_rule_filter_blocks_non_matching_device() {
        let yaml = r#"
groups:
  - keyboards:
      - vendor: "Apple"
    mappings:
      CapsLock: LeftControl
"#;
        let keyboards = vec![build_keyboard(
            "Logitech K845",
            "Logitech",
            "K845",
            "/dev/input/event5",
            Some("Bluetooth"),
        )];
        let state = build_state(yaml, keyboards);

        let result =
            state.global(HidUsage::CapsLock, 0, Some("/dev/input/event5"));
        assert!(result.is_none());
    }

    #[test]
    fn per_rule_filter_skipped_when_no_device_id() {
        // When device ID is None, per-rule filters are bypassed.
        let yaml = r#"
groups:
  - keyboards:
      - vendor: "Apple"
    mappings:
      CapsLock: LeftControl
"#;
        let state = build_state(yaml, vec![]);

        let result = state.global(HidUsage::CapsLock, 0, None);
        assert!(result.is_some());
    }

    // -----------------------------------------------------------------------
    // Combined global and per-rule filtering
    // -----------------------------------------------------------------------

    #[test]
    fn both_filters_applied_global_wins_first() {
        // The global filter is checked first.  Even if the per-rule filter
        // would pass, a failing global filter blocks everything.
        let yaml = r#"
keyboards:
  - vendor: "Logitech"
groups:
  - keyboards:
      - vendor: "Apple"
    mappings:
      CapsLock: LeftControl
"#;
        let keyboards = vec![build_keyboard(
            "Magic Keyboard",
            "Apple",
            "0x05ac",
            "/dev/input/event3",
            Some("USB"),
        )];
        let state = build_state(yaml, keyboards);

        // Global filter requires Logitech; this device is Apple.
        let result =
            state.global(HidUsage::CapsLock, 0, Some("/dev/input/event3"));
        assert!(result.is_none());
    }

    #[test]
    fn both_filters_pass_when_device_matches_all() {
        let yaml = r#"
keyboards:
  - vendor: "Apple"
groups:
  - keyboards:
      - name: "Magic Keyboard"
    mappings:
      CapsLock: LeftControl
"#;
        let keyboards = vec![build_keyboard(
            "Magic Keyboard",
            "Apple",
            "0x05ac",
            "/dev/input/event3",
            Some("USB"),
        )];
        let state = build_state(yaml, keyboards);

        // Matches both global (vendor=Apple) and per-rule (name=Magic
        // Keyboard).
        let result =
            state.global(HidUsage::CapsLock, 0, Some("/dev/input/event3"));
        assert!(result.is_some());
    }

    // -----------------------------------------------------------------------
    // First-match-wins with keyboard filtering
    // -----------------------------------------------------------------------

    #[test]
    fn first_match_wins_skips_filtered_rule() {
        // Two rules for CapsLock: the first is filtered to Apple keyboards,
        // the second has no filter.  When using a non-Apple keyboard, the
        // first rule is skipped and the second rule fires.
        let yaml = r#"
groups:
  - keyboards:
      - vendor: "Apple"
    mappings:
      CapsLock: LeftControl

  - mappings:
      CapsLock: LeftShift
"#;
        let keyboards = vec![build_keyboard(
            "Logitech K845",
            "Logitech",
            "K845",
            "/dev/input/event5",
            Some("Bluetooth"),
        )];
        let state = build_state(yaml, keyboards);

        // First rule is filtered out; second rule fires.
        let result =
            state.global(HidUsage::CapsLock, 0, Some("/dev/input/event5"));
        assert!(result.is_some());
        let result = result.unwrap();
        // Output is LeftShift (from the second rule), not LeftControl.
        assert_eq!(result[0].usage, HidUsage::LeftShift);
    }

    // -----------------------------------------------------------------------
    // App-scoped rules with keyboard filtering
    // -----------------------------------------------------------------------

    #[test]
    fn app_scoped_rule_filtered_by_keyboard() {
        let yaml = r#"
groups:
  - name: "myapp rules"
    apps: [MyApp]
    keyboards:
      - vendor: "Apple"
    mappings:
      A: B
"#;
        let keyboards = vec![build_keyboard(
            "Logitech K845",
            "Logitech",
            "K845",
            "/dev/input/event5",
            Some("Bluetooth"),
        )];
        let state = build_state(yaml, keyboards);

        // Device doesn't match the rule's keyboard filter.
        let result =
            state.for_app("MyApp", HidUsage::A, 0, Some("/dev/input/event5"));
        assert!(result.is_none());
    }

    #[test]
    fn keyboard_registry_stores_discovered_devices() {
        let yaml = r#"
groups:
  - mappings:
      A: B
"#;
        let keyboards = vec![
            build_keyboard("KB1", "Apple", "M1", "/dev/input/event3", None),
            build_keyboard(
                "KB2",
                "Logitech",
                "K845",
                "/dev/input/event5",
                None,
            ),
        ];
        let state = build_state(yaml, keyboards);

        assert!(state.resolve_keyboard("/dev/input/event3").is_some());
        assert!(state.resolve_keyboard("/dev/input/event5").is_some());
        assert!(state.resolve_keyboard("/dev/input/event99").is_none());
    }

    // The in-process mapping-engine integration cases that used to live here
    // — driven by a hand-rolled `simulate_mapping` loop that (incorrectly)
    // re-ran the lookup on key-up — now live in `keymap_core::engine::tests`,
    // expressed against `MappingEngine` directly so they exercise the real
    // key-fate model (a key-up's fate comes from its key-down's own record).
    // Keyboard filtering and app scoping are covered by the `RuntimeState`
    // tests above; the active-app cache moved to `super::focus` and its
    // TTL/single-flight mechanics are pinned in `common::ttl_value::tests`.
}
