// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use crate::mapping_cache::{NativeAction, NativeChord, RuntimeLookupCache};

/// Minimal interface for OS event-loop callbacks and state managers.
/// Deliberately small so that platform modules never learn about the
/// internal structure of [`RuntimeState`].
pub trait Lookup: Send + Sync {
    /// Best-effort lookup scoped to the given application name (lower-cased).
    ///
    /// `modifiers` is a bitmask of currently pressed modifier groups.
    /// Chord rules are checked first; single-key rules act as fallback.
    fn for_app(
        &self,
        app: &str,
        key: u16,
        modifiers: u8,
    ) -> Option<&NativeAction>;

    /// Global (application-agnostic) lookup.
    ///
    /// `modifiers` is a bitmask of currently pressed modifier groups.
    fn global(&self, key: u16, modifiers: u8) -> Option<&NativeAction>;

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
    fn for_app(
        &self,
        app: &str,
        key: u16,
        modifiers: u8,
    ) -> Option<&NativeAction> {
        // Check chord rules first (linear scan, first match wins).
        if let Some(chords) = self.lookup_cache.process_chords().get(app) {
            if let Some(action) = find_chord(chords, key, modifiers) {
                return Some(action);
            }
        }

        // Fall back to single-key HashMap lookup.
        self.lookup_cache
            .process_map()
            .get(app)
            .and_then(|m| m.get(&key))
    }

    fn global(&self, key: u16, modifiers: u8) -> Option<&NativeAction> {
        // Check chord rules first.
        if let Some(action) =
            find_chord(&self.lookup_cache.global_chords(), key, modifiers)
        {
            return Some(action);
        }

        // Fall back to single-key HashMap lookup.
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

/// Scan a list of chord rules and return the first match.
///
/// A rule matches when all its required modifier bits are present in the
/// pressed modifiers: `(pressed & required) == required`.
fn find_chord(
    chords: &[(NativeChord, NativeAction)],
    key: u16,
    modifiers: u8,
) -> Option<&NativeAction> {
    chords.iter().find_map(|(chord, action)| {
        if chord.base == key
            && (modifiers & chord.modifiers) == chord.modifiers
        {
            Some(action)
        } else {
            None
        }
    })
}
