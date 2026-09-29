// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Compile-time boundary test for the crate's public facade.
//!
//! Every `use` below names a path the `lib.rs` facade promises to keep
//! public.  An import that resolves is itself the assertion: if a facade item
//! is renamed, made private, or relocated, this file stops compiling —
//! turning an accidental public-API break into a clear failure.  Most imports
//! are therefore intentionally unused (hence the lint allow); a few cheap
//! operations are exercised to keep the test honest.
#![allow(unused_imports)]

use std::path::PathBuf;

#[cfg(target_os = "windows")]
use keymapper::platform::Key;
#[cfg(target_os = "macos")]
use keymapper::platform::{INJECTION_KEYBOARD_IDENTITY, KarabinerClient};
// Platform items that exist only on a single OS.
#[cfg(target_os = "linux")]
use keymapper::platform::{VIRTUAL_KEYBOARD_NAME, hid_translate};
use keymapper::{
    HidUsage,
    common::{
        config::{AppConfig, KeyEvent, RuleGroup},
        config_io::read_config_content,
        config_path::{
            default_config_path, find_config_path, find_config_path_strict,
        },
        hid_usage::PAGE_KEYBOARD,
        keyboard::{
            KeyboardInfo, KeyboardSpecifier, filter_keyboards_by_specifiers,
        },
    },
    daemon::{
        control, logging, state::RuntimeState, watcher::start_config_watcher,
    },
    keymap_core::{
        logfmt::Direction,
        lookup::{Lookup, MutableLookup},
        mapping_cache::{NativeKey, RuntimeLookupCache},
    },
    platform::{
        config_dir, keycode_to_hid_usage, list_keyboards, start_mapping,
    },
};

#[test]
fn public_facade_is_reachable() {
    // A few real operations, to keep the test from passing vacuously.
    let config = AppConfig::default();
    assert!(config.groups.is_empty(), "default config has no groups");
    let _ = config.check();

    let event = KeyEvent::parse("CapsLock").expect("parses a bare key");
    let group = RuleGroup {
        name: Some("g".to_string()),
        apps: Vec::new(),
        keyboards: Vec::new(),
        mappings: Default::default(),
    };
    assert!(!group.mappings.contains_key(&event));

    RuntimeLookupCache::compile_from_str("- mappings:\n    A: B\n")
        .expect("a minimal config compiles");

    // The crate-root re-export names the same type as its canonical home.
    let usage: HidUsage = keymapper::common::hid_usage::HidUsage::CapsLock;
    let _: HidUsage = usage;

    let _: Option<logging::LevelFilter> = None;
}
