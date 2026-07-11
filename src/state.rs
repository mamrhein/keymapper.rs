// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use crate::mapping_cache::{NativeAction, NativeKey, RuntimeLookupCache};

/// Minimal interface for OS event-loop callbacks and state managers.
/// Deliberately small so that platform modules never learn about the
/// internal structure of [`RuntimeState`].
pub trait Lookup: Send + Sync {
    /// Best-effort lookup scoped to the given application name (lower-cased).
    fn for_app(&self, app: &str, key: NativeKey) -> Option<&NativeAction>;

    /// Global (application-agnostic) lookup.
    fn global(&self, key: NativeKey) -> Option<&NativeAction>;

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
pub struct RuntimeState {
    lookup_cache: RuntimeLookupCache,
    active_app: String,
}

impl RuntimeState {
    pub(crate) fn new(cache: RuntimeLookupCache, app: String) -> Self {
        Self {
            lookup_cache: cache,
            active_app: app,
        }
    }
}

impl Lookup for RuntimeState {
    fn for_app(&self, app: &str, key: NativeKey) -> Option<&NativeAction> {
        self.lookup_cache
            .process_map()
            .get(app)
            .and_then(|m| m.get(&key))
    }

    fn global(&self, key: NativeKey) -> Option<&NativeAction> {
        self.lookup_cache.global_map().get(&key)
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
