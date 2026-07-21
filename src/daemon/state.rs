// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use super::mapping_cache::{CompiledRule, NativeKey, RuntimeLookupCache};

/// Minimal interface for OS event-loop callbacks and state managers.
/// Deliberately small so that platform modules never learn about the
/// internal structure of [`RuntimeState`].
pub trait Lookup: Send + Sync + std::fmt::Debug {
    /// Best-effort lookup scoped to the given application name.
    ///
    /// `modifiers` is the exact bitmask of currently pressed modifier keys.
    /// Returns the output events if a matching rule is found.
    fn for_app(
        &self,
        app: &str,
        key: u16,
        modifiers: u8,
    ) -> Option<&[NativeKey]>;

    /// Global (application-agnostic) lookup.
    fn global(&self, key: u16, modifiers: u8) -> Option<&[NativeKey]>;

    /// Name of the currently foreground application.
    fn active_app(&self) -> &str;

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
    active_app: String,
}

impl RuntimeState {
    pub fn new(cache: RuntimeLookupCache, app: String) -> Self {
        Self {
            lookup_cache: cache,
            active_app: app,
        }
    }
}

impl Lookup for RuntimeState {
    fn for_app(
        &self,
        app: &str,
        key: u16,
        modifiers: u8,
    ) -> Option<&[NativeKey]> {
        if let Some(rules) = self.lookup_cache.process_rules(app) {
            find_match(rules, key, modifiers)
        } else {
            None
        }
    }

    fn global(&self, key: u16, modifiers: u8) -> Option<&[NativeKey]> {
        find_match(self.lookup_cache.global_rules(), key, modifiers)
    }

    fn active_app(&self) -> &str {
        &self.active_app
    }

    fn set_active_app(&mut self, app: String) {
        self.active_app = app;
    }

    fn set_lookup_cache(&mut self, cache: RuntimeLookupCache) {
        self.lookup_cache = cache;
    }
}

/// Scan a list of compiled rules and return the first exact match.
fn find_match(
    rules: &[CompiledRule],
    key: u16,
    modifiers: u8,
) -> Option<&[NativeKey]> {
    rules.iter().find_map(|rule| {
        if rule.base == key && rule.modifiers == modifiers {
            Some(rule.outputs.as_slice())
        } else {
            None
        }
    })
}
