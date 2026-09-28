// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! The [`Lookup`] abstraction and the shared rule-matching helper.
//!
//! [`Lookup`] is the read-only interface the platform event taps call to
//! resolve a pressed key to its output events.  It is deliberately small so
//! the backends never learn about the internal structure of the live runtime
//! state; [`crate::daemon::state::RuntimeState`] is the production
//! implementation, [`TestLookup`](crate::keymap_core::test_lookup::TestLookup)
//! the in-process test one.

use crate::{
    common::{hid_usage::HidUsage, keyboard::KeyboardSpecifier},
    keymap_core::mapping_cache::{
        CompiledRule, NativeKey, RuntimeLookupCache,
    },
};

/// Read-only interface for OS event-loop callbacks and state managers.
/// Deliberately small so that platform modules never learn about the
/// internal structure of
/// [`RuntimeState`](crate::daemon::state::RuntimeState) or its mutation
/// operations.
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
/// [`RuntimeState`](crate::daemon::state::RuntimeState) has private fields.
pub trait MutableLookup: Lookup {
    /// Replace the compiled lookup cache (called by hot-reloader behind
    /// a write lock).
    fn set_lookup_cache(&mut self, cache: RuntimeLookupCache);
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
