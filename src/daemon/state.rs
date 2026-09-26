// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

use super::mapping_cache::{CompiledRule, NativeKey, RuntimeLookupCache};
use crate::common::{
    hid_usage::HidUsage,
    keyboard::{KeyboardInfo, KeyboardSpecifier},
};

/// How long a cached active-app name stays fresh.
///
/// The platform query is expensive (a synchronous D-Bus round-trip on
/// Wayland, potentially establishing a new connection), while keyboard
/// focus changes are rare.  A short TTL keeps per-event lookups cheap
/// without lagging focus switches in any perceptible way.
const ACTIVE_APP_TTL: Duration = Duration::from_millis(100);

/// Cached result of the most recent active-app query.
#[derive(Debug)]
struct CachedActiveApp {
    name: Arc<str>,
    queried_at: Instant,
}

/// Read-only interface for OS event-loop callbacks and state managers.
/// Deliberately small so that platform modules never learn about the
/// internal structure of [`RuntimeState`] or its mutation operations.
pub trait Lookup: Send + Sync + std::fmt::Debug {
    /// Best-effort lookup scoped to the given application name.
    ///
    /// `usage` is the HID identity of the pressed key.  `modifiers` is the
    /// exact bitmask of currently pressed modifier keys.
    /// `keyboard_device_id` is an optional platform-specific device
    /// identifier used for keyboard filtering.  Pass `None` when the
    /// platform cannot identify the source keyboard.
    ///
    /// Returns the output events if a matching rule is found.
    fn for_app(
        &self,
        app: &str,
        usage: HidUsage,
        modifiers: u8,
        keyboard_device_id: Option<&str>,
    ) -> Option<&[NativeKey]>;

    /// Best-effort lookup scoped to the currently active application.
    ///
    /// Resolves the active app name internally, so platform callers never
    /// have to fetch and thread it themselves.  The remaining arguments
    /// have the same meaning as in [`for_app`](Self::for_app).
    fn for_active_app(
        &self,
        usage: HidUsage,
        modifiers: u8,
        keyboard_device_id: Option<&str>,
    ) -> Option<&[NativeKey]>;

    /// Global (application-agnostic) lookup.
    ///
    /// `usage` is the HID identity of the pressed key.  `modifiers` is the
    /// exact bitmask of currently pressed modifier keys.
    /// `keyboard_device_id` is an optional platform-specific device
    /// identifier used for keyboard filtering.  Pass `None` when the
    /// platform cannot identify the source keyboard.
    fn global(
        &self,
        usage: HidUsage,
        modifiers: u8,
        keyboard_device_id: Option<&str>,
    ) -> Option<&[NativeKey]>;
}

/// Mutable operations on the runtime state.  Only the daemon internal code
/// (hot-reloader) needs this; platform modules depend solely on the read-only
/// [`Lookup`] trait.  External callers cannot implement this trait because
/// [`RuntimeState`] has private fields.
pub trait MutableLookup: Lookup {
    /// Replace the compiled lookup cache (called by hot-reloader behind
    /// a write lock).
    fn set_lookup_cache(&mut self, cache: RuntimeLookupCache);
}

/// Live runtime state shared between the config hot-reloader and the
/// platform-specific event tap.
pub struct RuntimeState {
    lookup_cache: RuntimeLookupCache,
    /// Maps platform device identifiers to full keyboard metadata.  Populated
    /// at startup from the platform's keyboard discovery and used to resolve
    /// device IDs to [`KeyboardInfo`] for keyboard filtering.
    keyboard_registry: HashMap<String, KeyboardInfo>,
    /// Short-TTL cache for the expensive active-app platform query, which is
    /// performed on every key event.  The lock is only ever held for the
    /// duration of a (cheap) field read or write, never across the platform
    /// query itself.
    active_app_cache: Mutex<CachedActiveApp>,
    /// Single-flight guard for the active-app refresh.  The thread that finds
    /// the cache expired holds this lock for the duration of the (blocking)
    /// platform query; concurrent callers serve the stale cache entry instead
    /// of queueing up behind a stalled IPC peer.  Without it, a slow
    /// compositor would block every engine thread on `active_app_cache` and
    /// freeze keyboard input system-wide.
    active_app_refresh: Mutex<()>,
    /// Injectable source for the active application name.  The daemon binary
    /// wires this to the platform query; tests can supply a fixed value.
    /// Kept as a closure so the state struct never references platform code
    /// directly.
    active_app_source: Box<dyn Fn() -> String + Send + Sync>,
}

impl std::fmt::Debug for RuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeState")
            .field("lookup_cache", &self.lookup_cache)
            .field("keyboard_registry", &self.keyboard_registry)
            .field("active_app_cache", &self.active_app_cache)
            .field("active_app_refresh", &self.active_app_refresh)
            .field("active_app_source", &"<fn>")
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
            // Start stale-on-purpose: the first key event within
            // ACTIVE_APP_TTL of daemon startup uses "unknown", which only
            // skips app-scoped rules for that brief window.
            active_app_cache: Mutex::new(CachedActiveApp {
                name: Arc::from("unknown"),
                queried_at: Instant::now(),
            }),
            active_app_refresh: Mutex::new(()),
            active_app_source,
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

    /// Name of the currently foreground application, served from a short-TTL
    /// cache so per-event lookups stay cheap.
    ///
    /// The refresh is single-flight and the blocking platform query (X11
    /// round-trip, D-Bus call, compositor socket) runs _outside_ the cache
    /// lock: the first caller to find the entry expired claims the refresh
    /// slot and updates the cache when the query returns, while any other
    /// caller immediately receives the last known value.  A stalled IPC peer
    /// can therefore at worst cause brief staleness, never a lock-up of the
    /// keystroke hot path.  If the query panics, the guard's `Drop` releases
    /// the refresh slot, so the cache cannot be wedged permanently.
    fn active_app(&self) -> Arc<str> {
        {
            let cache = self.active_app_cache.lock();
            if cache.queried_at.elapsed() < ACTIVE_APP_TTL {
                return Arc::clone(&cache.name);
            }
        }
        // The entry is stale.  Serve it while a single thread refreshes.
        let Some(_refresh_slot) = self.active_app_refresh.try_lock() else {
            return Arc::clone(&self.active_app_cache.lock().name);
        };
        let name: Arc<str> = (self.active_app_source)().into();
        let mut cache = self.active_app_cache.lock();
        cache.name = Arc::clone(&name);
        cache.queried_at = Instant::now();
        name
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
        self.for_app(&self.active_app(), usage, modifiers, keyboard_device_id)
    }
}

impl MutableLookup for RuntimeState {
    fn set_lookup_cache(&mut self, cache: RuntimeLookupCache) {
        self.lookup_cache = cache;
    }
}

/// Scan a list of compiled rules and return the first exact match.
///
/// `check_keyboard` is called for each matching rule to verify its per-rule
/// keyboard filter.  It should return `true` if the rule is allowed to
/// fire for the current keyboard device.
pub(crate) fn find_match<F>(
    rules: &[CompiledRule],
    usage: HidUsage,
    modifiers: u8,
    check_keyboard: F,
) -> Option<&[NativeKey]>
where
    F: Fn(&Option<Vec<KeyboardSpecifier>>) -> bool,
{
    rules.iter().find_map(|rule| {
        if rule.usage == usage
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
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };

    use super::*;
    use crate::{
        common::{config::AppConfig, hid_usage::HidUsage},
        daemon::test_lookup::TestLookup,
    };

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

    /// Simulate a sequence of key events through the lookup engine.
    ///
    /// `events` are `(HidUsage, is_down)` pairs.  Modifier tracking and
    /// keyboard filtering semantics match the real event-loop callback.
    /// Returns the sequence of [`NativeKey`] outputs emitted for each
    /// input event.
    fn simulate_mapping(
        lookup: &dyn Lookup,
        events: &[(HidUsage, bool)],
    ) -> Vec<Option<Vec<NativeKey>>> {
        let mut modifier_state: u8 = 0;
        let mut results = Vec::new();

        for &(usage, is_down) in events {
            // Map the HID usage to its modifier bit, if it is a modifier.
            let modifier_bit = HidUsage::hid_usage_to_modifier_bit(usage);

            // Capture modifier state before updating (for concurrent
            // matching).
            let lookup_modifiers = modifier_state;

            // Update modifier tracking.
            if let Some(bit) = modifier_bit {
                if is_down {
                    modifier_state |= 1 << bit;
                } else {
                    modifier_state &= !(1 << bit);
                }
            }

            // Perform lookup.
            let active_outputs = lookup
                .for_active_app(usage, lookup_modifiers, None)
                .or_else(|| lookup.global(usage, lookup_modifiers, None))
                .map(|v| v.to_vec());

            // Undo modifier contribution if the key was swallowed.
            if active_outputs.is_some()
                && is_down
                && let Some(bit) = modifier_bit
            {
                modifier_state &= !(1 << bit);
            }

            results.push(active_outputs);
        }

        results
    }

    // -----------------------------------------------------------------------
    // In-process mapping engine integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn in_process_capslock_to_control() {
        let yaml = r#"- mappings:
    CapsLock: LeftControl"#;
        let lookup = TestLookup::from_yaml(yaml);

        let results = simulate_mapping(
            &lookup,
            &[(HidUsage::CapsLock, true), (HidUsage::CapsLock, false)],
        );

        // Both events should be remapped to LeftControl.
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0],
            Some(vec![NativeKey {
                modifiers: 0,
                usage: HidUsage::LeftControl,
            }])
        );
        assert_eq!(
            results[1],
            Some(vec![NativeKey {
                modifiers: 0,
                usage: HidUsage::LeftControl,
            }])
        );
    }

    #[test]
    fn in_process_unmapped_passthrough() {
        let yaml = r#"- mappings:
    CapsLock: LeftControl"#;
        let lookup = TestLookup::from_yaml(yaml);

        let results = simulate_mapping(
            &lookup,
            &[(HidUsage::A, true), (HidUsage::A, false)],
        );

        // 'A' has no mapping, so it passes through.
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], None);
        assert_eq!(results[1], None);
    }

    #[test]
    fn in_process_simple_remap() {
        let yaml = r#"- mappings:
    A: B"#;
        let lookup = TestLookup::from_yaml(yaml);

        let results = simulate_mapping(
            &lookup,
            &[(HidUsage::A, true), (HidUsage::A, false)],
        );

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0],
            Some(vec![NativeKey {
                modifiers: 0,
                usage: HidUsage::B,
            }])
        );
        assert_eq!(
            results[1],
            Some(vec![NativeKey {
                modifiers: 0,
                usage: HidUsage::B,
            }])
        );
    }

    #[test]
    fn in_process_chord_output() {
        let yaml = r#"- mappings:
    CapsLock: Cmd+A"#;
        let lookup = TestLookup::from_yaml(yaml);

        let results = simulate_mapping(
            &lookup,
            &[(HidUsage::CapsLock, true), (HidUsage::CapsLock, false)],
        );

        assert_eq!(results.len(), 2);
        // Key-down produces one chord output: Cmd+A.
        let down_outputs = results[0].as_ref().unwrap();
        assert_eq!(down_outputs.len(), 1);
        let chord = &down_outputs[0];
        assert_eq!(chord.usage, HidUsage::A);
        // Cmd modifier bit should be set (bit 3 for LeftCommand).
        assert!((chord.modifiers & (1 << 3)) != 0);

        // Key-up produces the same chord release.
        let up_outputs = results[1].as_ref().unwrap();
        assert_eq!(up_outputs.len(), 1);
    }

    #[test]
    fn in_process_multi_output() {
        let yaml = r#"- mappings:
    CapsLock: [LeftControl, A]"#;
        let lookup = TestLookup::from_yaml(yaml);

        let results = simulate_mapping(
            &lookup,
            &[(HidUsage::CapsLock, true), (HidUsage::CapsLock, false)],
        );

        assert_eq!(results.len(), 2);
        // Key-down produces two outputs: LeftControl then A.
        let down_outputs = results[0].as_ref().unwrap();
        assert_eq!(down_outputs.len(), 2);
        assert_eq!(down_outputs[0].usage, HidUsage::LeftControl);
        assert_eq!(down_outputs[1].usage, HidUsage::A);

        // Key-up produces the same two outputs.
        let up_outputs = results[1].as_ref().unwrap();
        assert_eq!(up_outputs.len(), 2);
    }

    #[test]
    fn in_process_modifier_combination() {
        let yaml = r#"- mappings:
    Ctrl+A: B"#;
        let lookup = TestLookup::from_yaml(yaml);

        let results = simulate_mapping(
            &lookup,
            &[
                (HidUsage::LeftControl, true),  // Ctrl down
                (HidUsage::A, true),            // A down (with Ctrl)
                (HidUsage::A, false),           // A up
                (HidUsage::LeftControl, false), // Ctrl up
            ],
        );

        assert_eq!(results.len(), 4);
        // Ctrl down passes through (no modifier in its lookup_modifiers).
        assert_eq!(results[0], None);
        // A down with Ctrl modifier is remapped to B.
        assert_eq!(
            results[1],
            Some(vec![NativeKey {
                modifiers: 0,
                usage: HidUsage::B,
            }])
        );
        // A up is also remapped (key-up of the trigger).
        assert_eq!(
            results[2],
            Some(vec![NativeKey {
                modifiers: 0,
                usage: HidUsage::B,
            }])
        );
        // Ctrl up passes through.
        assert_eq!(results[3], None);
    }

    #[test]
    fn in_process_swap_mapping() {
        let yaml = r#"- mappings:
    CapsLock: LeftControl
    LeftControl: CapsLock"#;
        let lookup = TestLookup::from_yaml(yaml);

        // CapsLock -> LeftControl.
        let results = simulate_mapping(
            &lookup,
            &[(HidUsage::CapsLock, true), (HidUsage::CapsLock, false)],
        );

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0],
            Some(vec![NativeKey {
                modifiers: 0,
                usage: HidUsage::LeftControl,
            }])
        );
        assert_eq!(
            results[1],
            Some(vec![NativeKey {
                modifiers: 0,
                usage: HidUsage::LeftControl,
            }])
        );

        // LeftControl -> CapsLock.
        let results = simulate_mapping(
            &lookup,
            &[
                (HidUsage::LeftControl, true),
                (HidUsage::LeftControl, false),
            ],
        );

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0],
            Some(vec![NativeKey {
                modifiers: 0,
                usage: HidUsage::CapsLock,
            }])
        );
        assert_eq!(
            results[1],
            Some(vec![NativeKey {
                modifiers: 0,
                usage: HidUsage::CapsLock,
            }])
        );
    }

    #[test]
    fn in_process_modifier_passthrough_with_remap() {
        // When CapsLock is remapped to LeftControl, pressing it then 'A'
        // should produce: Ctrl+A (the remapped modifier + the letter).
        let yaml = r#"- mappings:
    CapsLock: LeftControl"#;
        let lookup = TestLookup::from_yaml(yaml);

        let results = simulate_mapping(
            &lookup,
            &[
                (HidUsage::CapsLock, true),  // mapped to Ctrl
                (HidUsage::A, true),         // A with Ctrl modifier
                (HidUsage::A, false),        // A up
                (HidUsage::CapsLock, false), // Ctrl up
            ],
        );

        assert_eq!(results.len(), 4);
        // CapsLock down is mapped to Ctrl.
        assert!(results[0].is_some());
        // 'A' with Ctrl modifier passes through (no rule for Ctrl+A).
        assert_eq!(results[1], None);
        // 'A' up passes through.
        assert_eq!(results[2], None);
        // CapsLock up is mapped to Ctrl up.
        assert!(results[3].is_some());
    }

    // -----------------------------------------------------------------------
    // Active-app cache
    // -----------------------------------------------------------------------

    #[test]
    fn active_app_serves_fresh_entries_without_querying() {
        let calls = Arc::new(AtomicUsize::new(0));
        let src_calls = Arc::clone(&calls);
        let state = RuntimeState::new(
            RuntimeLookupCache::compile_from_config(&AppConfig::default()),
            Vec::new(),
            Box::new(move || {
                src_calls.fetch_add(1, Ordering::SeqCst);
                "queried_app".to_string()
            }),
        );

        // The entry seeded by `new` is fresh, so no query runs.
        assert_eq!(&*state.active_app(), "unknown");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn active_app_refresh_publishes_and_is_cached() {
        let calls = Arc::new(AtomicUsize::new(0));
        let src_calls = Arc::clone(&calls);
        let state = RuntimeState::new(
            RuntimeLookupCache::compile_from_config(&AppConfig::default()),
            Vec::new(),
            Box::new(move || {
                let n = src_calls.fetch_add(1, Ordering::SeqCst);
                format!("app{n}")
            }),
        );
        expire_active_app_cache(&state);

        assert_eq!(&*state.active_app(), "app0");
        // Serving the refreshed entry within the TTL must not re-query.
        assert_eq!(&*state.active_app(), "app0");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A stalled platform query must not block concurrent callers: exactly
    /// one thread runs the refresh (outside the cache lock) while the others
    /// immediately serve the stale entry.
    #[test]
    fn active_app_stalled_refresh_does_not_block_callers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let src_calls = Arc::clone(&calls);
        // Signals set by the source when a query starts and by the test when
        // the stalled query may finish.  `release_main` releases the one
        // blocking refresh; `release_others` lets any *unexpected* second
        // query return promptly instead of hanging the test forever.
        let query_started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let src_started = Arc::clone(&query_started);
        let src_release = Arc::clone(&release);

        let state = RuntimeState::new(
            RuntimeLookupCache::compile_from_config(&AppConfig::default()),
            Vec::new(),
            Box::new(move || {
                src_calls.fetch_add(1, Ordering::SeqCst);
                src_started.store(true, Ordering::SeqCst);
                // Simulate a compositor or IPC peer that stops responding.
                while !src_release.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                "fresh_app".to_string()
            }),
        );
        expire_active_app_cache(&state);

        std::thread::scope(|s| {
            let refresher = s.spawn(|| state.active_app());

            // Wait until the refresh is genuinely in flight, i.e. the source
            // has been entered (which happens while the refresh slot, but
            // not the cache lock, is held).
            while !query_started.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }

            // Concurrent callers must serve the stale value immediately
            // rather than queueing behind the stalled query.
            let start = Instant::now();
            let served = state.active_app();
            let elapsed = start.elapsed();
            assert_eq!(&*served, "unknown");
            assert!(
                elapsed < ACTIVE_APP_TTL,
                "caller blocked behind the stalled refresh ({elapsed:?})",
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "refresh is not single-flight",
            );

            // Let the stalled query finish, then verify its result got
            // published.
            release.store(true, Ordering::SeqCst);
            assert_eq!(&*refresher.join().unwrap(), "fresh_app");
        });
        assert_eq!(&*state.active_app(), "fresh_app");
    }

    /// Hammer the cache from many threads while the platform query is slow:
    /// concurrent lookups must never deadlock and never run more than one
    /// query at a time.  The in-flight assertion lives inside the source so
    /// it holds regardless of how the threads interleave.
    #[test]
    fn active_app_concurrent_stale_reads_do_not_deadlock() {
        const N_READERS: usize = 8;
        const N_READS: usize = 500;

        let queries = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let started_tx = Arc::new(started_tx);

        let src_queries = Arc::clone(&queries);
        let src_in_flight = Arc::clone(&in_flight);
        let src_started_tx = Arc::clone(&started_tx);
        let state = Arc::new(RuntimeState::new(
            RuntimeLookupCache::compile_from_config(&AppConfig::default()),
            Vec::new(),
            Box::new(move || {
                src_queries.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    src_in_flight.fetch_add(1, Ordering::SeqCst),
                    0,
                    "two active-app queries ran concurrently",
                );
                // Unbounded channel: `send` never blocks.
                let _ = src_started_tx.send(());
                // Simulate a slow compositor round-trip.
                std::thread::sleep(Duration::from_millis(20));
                src_in_flight.fetch_sub(1, Ordering::SeqCst);
                "slow_app".to_string()
            }),
        ));
        expire_active_app_cache(&state);

        let readers: Vec<_> = (0..N_READERS)
            .map(|_| {
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
                    for _ in 0..N_READS {
                        let name = state.active_app();
                        assert!(!name.is_empty());
                    }
                })
            })
            .collect();

        // At least one refresh was in flight while the readers hammered; the
        // readers finishing without deadlock is verified by the joins.
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("no refresh ever started");
        for reader in readers {
            reader.join().unwrap();
        }
        assert!(queries.load(Ordering::SeqCst) >= 1);
    }

    /// Force the active-app cache entry stale without waiting for the real
    /// TTL to elapse.
    fn expire_active_app_cache(state: &RuntimeState) {
        // `Instant::now() - ACTIVE_APP_TTL` cannot underflow in practice
        // (the monotonic clock starts at boot).
        state.active_app_cache.lock().queried_at =
            Instant::now() - ACTIVE_APP_TTL * 2;
    }
}
