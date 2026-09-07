// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Shared in-process [`Lookup`] harness for daemon unit tests.
//!
//! [`TestLookup`] is a simple [`Lookup`] implementation backed by a
//! [`RuntimeLookupCache`].  It resolves the active app to a fixed name without
//! querying the platform, so tests that drive the mapping engine (the decision
//! core and the runtime state) stay deterministic and free of platform
//! round-trips.

use crate::{
    common::{config::AppConfig, hid_usage::HidUsage},
    daemon::{
        mapping_cache::{NativeKey, RuntimeLookupCache},
        state::{Lookup, find_match},
    },
};

/// A simple [`Lookup`] implementation backed by a [`RuntimeLookupCache`]
/// for in-process testing.  Resolves the active app to the configured
/// name without querying the platform.
pub(crate) struct TestLookup {
    cache: RuntimeLookupCache,
    app_name: String,
}

impl TestLookup {
    /// Compile a [`TestLookup`] from a YAML config string.
    pub(crate) fn from_yaml(yaml: &str) -> Self {
        let config = AppConfig::load_from_str(yaml).unwrap();
        Self {
            cache: RuntimeLookupCache::compile_from_config(&config),
            app_name: "test_app".to_string(),
        }
    }
}

impl std::fmt::Debug for TestLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestLookup").finish()
    }
}

impl Lookup for TestLookup {
    fn for_app(
        &self,
        app: &str,
        usage: HidUsage,
        modifiers: u8,
        _kbd_device_id: Option<&str>,
    ) -> Option<&[NativeKey]> {
        // For tests we always check the global rules; app-scoped
        // rules are tested via the RuntimeState tests.
        if let Some(rules) = self.cache.process_rules(app) {
            find_match(rules, usage, modifiers, |_rule_keyboards| true)
        } else {
            None
        }
    }

    fn global(
        &self,
        usage: HidUsage,
        modifiers: u8,
        _kbd_id: Option<&str>,
    ) -> Option<&[NativeKey]> {
        find_match(
            self.cache.global_rules(),
            usage,
            modifiers,
            |_rule_keyboards| true,
        )
    }

    fn for_active_app(
        &self,
        usage: HidUsage,
        modifiers: u8,
        kbd_device_id: Option<&str>,
    ) -> Option<&[NativeKey]> {
        self.for_app(&self.app_name, usage, modifiers, kbd_device_id)
    }
}
