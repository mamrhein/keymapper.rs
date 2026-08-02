// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{collections::HashMap, sync::Arc};

use super::mapping_cache::{CompiledRule, NativeKey, RuntimeLookupCache};
use crate::common::keyboard::{KeyboardInfo, KeyboardSpecifier};

/// Read-only interface for OS event-loop callbacks and state managers.
/// Deliberately small so that platform modules never learn about the
/// internal structure of [`RuntimeState`] or its mutation operations.
pub trait Lookup: Send + Sync + std::fmt::Debug {
    /// Best-effort lookup scoped to the given application name.
    ///
    /// `modifiers` is the exact bitmask of currently pressed modifier keys.
    /// `keyboard_device_id` is an optional platform-specific device
    /// identifier used for keyboard filtering.  Pass `None` when the
    /// platform cannot identify the source keyboard.
    ///
    /// Returns the output events if a matching rule is found.
    fn for_app(
        &self,
        app: &str,
        key: u16,
        modifiers: u8,
        keyboard_device_id: Option<&str>,
    ) -> Option<&[NativeKey]>;

    /// Global (application-agnostic) lookup.
    ///
    /// `keyboard_device_id` is an optional platform-specific device
    /// identifier used for keyboard filtering.  Pass `None` when the
    /// platform cannot identify the source keyboard.
    fn global(
        &self,
        key: u16,
        modifiers: u8,
        keyboard_device_id: Option<&str>,
    ) -> Option<&[NativeKey]>;

    /// Name of the currently foreground application.  Returns an
    /// `Arc<str>` so callers can read without cloning.
    fn active_app(&self) -> &Arc<str>;
}

/// Mutable operations on the runtime state.  Only the daemon internal code
/// (hot-reloader and app tracker) needs this; platform modules depend solely
/// on the read-only [`Lookup`] trait.  External callers cannot implement this
/// trait because [`RuntimeState`] has private fields.
pub trait MutableLookup: Lookup {
    /// Update the foreground application name (called behind a write lock).
    fn set_active_app(&mut self, app: String);

    /// Replace the compiled lookup cache (called by hot-reloader behind
    /// a write lock).
    fn set_lookup_cache(&mut self, cache: RuntimeLookupCache);
}

/// Live runtime state shared between the config hot-reloader, the foreground-
/// app tracker, and the platform-specific event tap.
#[derive(Debug)]
pub struct RuntimeState {
    lookup_cache: RuntimeLookupCache,
    active_app: Arc<str>,
    /// Maps platform device identifiers to full keyboard metadata.  Populated
    /// at startup from the platform's keyboard discovery and used to resolve
    /// device IDs to [`KeyboardInfo`] for keyboard filtering.
    keyboard_registry: HashMap<String, KeyboardInfo>,
}

impl RuntimeState {
    pub fn new(
        cache: RuntimeLookupCache,
        app: String,
        keyboards: Vec<KeyboardInfo>,
    ) -> Self {
        Self {
            lookup_cache: cache,
            active_app: app.into(),
            keyboard_registry: keyboards
                .into_iter()
                .map(|kb| (kb.device.clone(), kb))
                .collect(),
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
        key: u16,
        modifiers: u8,
        keyboard_device_id: Option<&str>,
    ) -> Option<&[NativeKey]> {
        // Check the global filter first.
        if !self.check_global_keyboard_filter(keyboard_device_id) {
            return None;
        }

        if let Some(rules) = self.lookup_cache.process_rules(app) {
            find_match(rules, key, modifiers, |rule_keyboards| {
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
        key: u16,
        modifiers: u8,
        keyboard_device_id: Option<&str>,
    ) -> Option<&[NativeKey]> {
        // Check the global filter first.
        if !self.check_global_keyboard_filter(keyboard_device_id) {
            return None;
        }

        find_match(
            self.lookup_cache.global_rules(),
            key,
            modifiers,
            |rule_keyboards| {
                self.check_rule_keyboard_filter(
                    rule_keyboards,
                    keyboard_device_id,
                )
            },
        )
    }

    fn active_app(&self) -> &Arc<str> {
        &self.active_app
    }
}

impl MutableLookup for RuntimeState {
    fn set_active_app(&mut self, app: String) {
        self.active_app = app.into();
    }

    fn set_lookup_cache(&mut self, cache: RuntimeLookupCache) {
        self.lookup_cache = cache;
    }
}

/// Scan a list of compiled rules and return the first exact match.
///
/// `check_keyboard` is called for each matching rule to verify its per-rule
/// keyboard filter.  It should return `true` if the rule is allowed to
/// fire for the current keyboard device.
fn find_match<F>(
    rules: &[CompiledRule],
    key: u16,
    modifiers: u8,
    check_keyboard: F,
) -> Option<&[NativeKey]>
where
    F: Fn(&Option<Vec<KeyboardSpecifier>>) -> bool,
{
    rules.iter().find_map(|rule| {
        if rule.base == key
            && rule.modifiers == modifiers
            && check_keyboard(&rule.keyboards)
        {
            Some(rule.outputs.as_slice())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{common::config::AppConfig, platform::Key};

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
        RuntimeState::new(cache, "unknown".to_string(), keyboards)
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
        let result = state.global(Key::CapsLock.as_native(), 0, None);
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
        let result = state.global(
            Key::CapsLock.as_native(),
            0,
            Some("/dev/input/event3"),
        );
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
        let result = state.global(
            Key::CapsLock.as_native(),
            0,
            Some("/dev/input/event5"),
        );
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
        let result = state.global(Key::CapsLock.as_native(), 0, None);
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
        let result = state.global(
            Key::CapsLock.as_native(),
            0,
            Some("/dev/input/event99"),
        );
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

        let result = state.global(
            Key::CapsLock.as_native(),
            0,
            Some("/dev/input/event3"),
        );
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

        let result = state.global(
            Key::CapsLock.as_native(),
            0,
            Some("/dev/input/event5"),
        );
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

        let result = state.global(Key::CapsLock.as_native(), 0, None);
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
        let result = state.global(
            Key::CapsLock.as_native(),
            0,
            Some("/dev/input/event3"),
        );
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
        let result = state.global(
            Key::CapsLock.as_native(),
            0,
            Some("/dev/input/event3"),
        );
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
        let result = state.global(
            Key::CapsLock.as_native(),
            0,
            Some("/dev/input/event5"),
        );
        assert!(result.is_some());
        let result = result.unwrap();
        // Output is LeftShift (from the second rule), not LeftControl.
        assert_eq!(result[0].base, Key::LeftShift.as_native());
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
        let result = state.for_app(
            "MyApp",
            Key::A.as_native(),
            0,
            Some("/dev/input/event5"),
        );
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
}
