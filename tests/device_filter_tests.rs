// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Cross-platform pin for the device-filter rule semantics.
//!
//! Keyboard filtering is one engine shared by three platform front-ends
//! that feed it different device-identification inputs:
//!
//! - Linux always identifies the source (the grabbed evdev path) and also
//!   applies the document-level `keyboards` filter at the capture level,
//!   selecting which devices get grabbed.
//! - Windows identifies the source via raw input for most presses but ignores
//!   the capture-level filter (the hook is session-global).
//! - macOS never identifies the source (CGEvents carry no device), so every
//!   lookup arrives with `device_id = None`.
//!
//! The single test below drives the shared public lookup surface with the
//! device-id availability each platform produces, pinning the documented
//! contract: filters are skipped when the source is unidentified, the global
//! filter gates the whole lookup before any rule is considered, unknown
//! devices fail open, and a filter-miss rule is skipped under
//! first-match-wins.  It also pins the capture-level grab-set selection
//! ([`filter_keyboards_by_specifiers`]) that only Linux applies.

use keymapper::{
    HidUsage,
    common::keyboard::{
        KeyboardInfo, KeyboardSpecifier, filter_keyboards_by_specifiers,
    },
    daemon::state::RuntimeState,
    keymap_core::{lookup::Lookup, mapping_cache::RuntimeLookupCache},
};

/// A config with a global filter (vendor, lowercased to pin case-insensitive
/// matching), one keyboard-scoped group, and an unscoped fallback group for
/// the same trigger so first-match-wins behavior is observable.
const YAML: &str = r#"
keyboards:
  - vendor: apple
groups:
  - name: scoped
    keyboards:
      - name: magic keyboard
        port: usb
    mappings:
      CapsLock: LeftControl
  - name: fallback
    mappings:
      CapsLock: LeftShift
"#;

fn keyboard(
    name: &str,
    vendor: &str,
    device: &str,
    port: Option<&str>,
) -> KeyboardInfo {
    KeyboardInfo::new(
        name.to_string(),
        vendor.to_string(),
        "0x0000".to_string(),
        device.to_string(),
        port.map(str::to_string),
    )
}

fn spec(vendor: Option<&str>, name: Option<&str>) -> KeyboardSpecifier {
    KeyboardSpecifier {
        name: name.map(str::to_string),
        vendor: vendor.map(str::to_string),
        model: None,
        port: None,
    }
}

#[test]
fn device_filter_rule_semantics_per_platform() {
    // Registered keyboards spanning the three filter outcomes: `builtin`
    // passes both filters, `track` passes only the global filter, and
    // `external` fails the global filter.
    let keyboards = vec![
        keyboard("Magic Keyboard", "Apple", "dev-builtin", Some("USB")),
        keyboard("Track Keyboard", "Apple", "dev-track", Some("Bluetooth")),
        keyboard("K845", "Logitech", "dev-external", Some("Bluetooth")),
    ];

    // -- Platform-independent filter compilation --------------------------

    let cache = RuntimeLookupCache::compile_from_str(YAML)
        .expect("the pinned config compiles");
    assert_eq!(
        cache.global_keyboards().map(Vec::len),
        Some(1),
        "a non-empty document-level `keyboards` list compiles to a global \
         filter"
    );
    let empty: &str =
        "keyboards: []\ngroups:\n  - mappings:\n      CapsLock: LeftControl\n";
    let cache_empty = RuntimeLookupCache::compile_from_str(empty)
        .expect("a config with an empty global filter compiles");
    assert!(
        cache_empty.global_keyboards().is_none(),
        "an empty `keyboards` list means no filter on every platform"
    );

    // Clone the compiled global filter before the cache moves into the
    // runtime state; the capture-level section below needs it.
    let global_filter = cache.global_keyboards().cloned();
    let state = RuntimeState::new(
        cache,
        keyboards.clone(),
        Box::new(|| "test_app".to_string()),
    );

    // -- macOS semantics: source never identified (`device_id = None`) ----
    //
    // CGEvents expose no originating device, so every macOS lookup arrives
    // with `None`.  Both the global filter and the scoped group's filter
    // must be skipped rather than block: the first rule fires even though
    // its filter cannot be evaluated.  This is also the Windows fallback
    // when raw-input matching never found the source device.

    let out = state
        .global(HidUsage::CapsLock, 0, None)
        .expect("filters are skipped when the source is unidentified");
    assert_eq!(
        out[0].usage,
        HidUsage::LeftControl,
        "with filters skipped the first rule wins, not the fallback"
    );

    // -- Linux and Windows semantics: source identified (`Some(id)`) ------

    // A device matching both filters fires the scoped rule.  The filter
    // fields ("apple", "magic keyboard", "usb") match case-insensitively
    // and a multi-field specifier requires all fields to match.
    let out = state
        .global(HidUsage::CapsLock, 0, Some("dev-builtin"))
        .expect("a fully matching device fires the scoped rule");
    assert_eq!(out[0].usage, HidUsage::LeftControl);

    // A device that passes the global filter but fails the scoped group's
    // filter skips that rule; first-match-wins then fires the unscoped
    // fallback instead of blocking the key.
    let out = state
        .global(HidUsage::CapsLock, 0, Some("dev-track"))
        .expect("the unscoped fallback fires for a scoped-rule miss");
    assert_eq!(out[0].usage, HidUsage::LeftShift);

    // A device failing the global filter gets nothing: the global filter is
    // checked first and gates the entire lookup, so even the unscoped
    // fallback cannot fire.  (On Windows this only gates rule lookup — the
    // session-global hook still observes every keyboard; on Linux the
    // device would additionally never have been grabbed, see below.)
    assert!(
        state
            .global(HidUsage::CapsLock, 0, Some("dev-external"))
            .is_none(),
        "a device failing the global filter must not match any rule"
    );

    // An unidentified-but-known-absent device fails open: a device that is
    // not in the registry (Linux hot-plug before the registry knows it, a
    // Windows raw-input path that failed to resolve) keeps its mappings to
    // avoid silently dropping keys.
    let out = state
        .global(HidUsage::CapsLock, 0, Some("dev-unknown"))
        .expect("unknown devices fail open");
    assert_eq!(out[0].usage, HidUsage::LeftControl);

    // -- Capture-level grab-set selection (Linux only) --------------------
    //
    // Only Linux passes the global filter into `start_mapping` to restrict
    // which devices are grabbed; Windows and macOS ignore the argument
    // (their capture is session-global).  The selection semantics below are
    // the ones the Linux daemon and its hot-plug monitor apply.

    // No filter (or an empty one) selects every device.
    assert_eq!(
        filter_keyboards_by_specifiers(&keyboards, None).len(),
        keyboards.len(),
        "no global filter grabs every keyboard"
    );

    // The global filter selects exactly the matching devices.
    let grabbed =
        filter_keyboards_by_specifiers(&keyboards, global_filter.as_deref());
    let grabbed_names: Vec<&str> =
        grabbed.iter().map(|kb| kb.name.as_str()).collect();
    assert_eq!(grabbed_names, ["Magic Keyboard", "Track Keyboard"]);

    // Multiple specifiers form an OR set.
    let grabbed = filter_keyboards_by_specifiers(
        &keyboards,
        Some(&[spec(Some("apple"), None), spec(Some("Logitech"), None)]),
    );
    assert_eq!(grabbed.len(), 3, "OR of apple and Logitech takes all");

    // A filter nothing matches selects no device.
    let grabbed = filter_keyboards_by_specifiers(
        &keyboards,
        Some(&[spec(Some("Microsoft"), None)]),
    );
    assert!(
        grabbed.is_empty(),
        "a filter no device matches grabs nothing"
    );
}
