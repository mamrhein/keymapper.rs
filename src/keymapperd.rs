// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::sync::Arc;

// Import Lookup so read-only trait methods are in scope, and
// MutableLookup so mutation methods are callable on the concrete
// RuntimeState type.
use keymapper::daemon::state::{Lookup, MutableLookup};
use parking_lot::RwLock;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = keymapper::common::config_path::find_config_path_strict(
    )
    .map_err(|e| {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    })?;

    // Resolve to an absolute path so the watcher and cache compiler have
    // a stable reference regardless of later CWD changes.  Symlinks in
    // parent directory components are resolved here; the config file itself
    // was already verified to not be a symlink.
    let config_path = config_path.canonicalize().unwrap_or(config_path);

    let initial_cache =
        keymapper::daemon::mapping_cache::RuntimeLookupCache::compile_from_path(
            &config_path,
        )?;

    // Discover connected keyboards to populate the device registry.  The
    // registry gets ALL keyboards so that filtering at rule-level can resolve
    // any device.
    //
    // On Linux, discover_and_open_keyboards performs a single udev scan and
    // keeps the devices open, avoiding a redundant open in start_mapping.
    #[cfg(target_os = "linux")]
    let (all_keyboards, opened_devices) = {
        let opened = keymapper::platform::discover_and_open_keyboards()
            .unwrap_or_default();
        let infos: Vec<keymapper::common::keyboard::KeyboardInfo> = opened
            .iter()
            .map(|(info, _)| info.clone())
            .collect();
        (infos, opened)
    };

    #[cfg(not(target_os = "linux"))]
    let all_keyboards =
        keymapper::platform::list_keyboards().unwrap_or_default();

    // Determine which keyboards to actually grab based on the global filter.
    // Only matching keyboards are captured; others work normally.
    let keyboards_to_grab: Vec<keymapper::common::keyboard::KeyboardInfo> =
        keymapper::common::keyboard::filter_keyboards_by_specifiers(
            &all_keyboards,
            initial_cache.global_keyboards().map(Vec::as_slice),
        );

    if !keyboards_to_grab.is_empty() {
        println!(
            "Grabbing {} keyboard(s) ({} total discovered):",
            keyboards_to_grab.len(),
            all_keyboards.len()
        );
        for kb in &keyboards_to_grab {
            println!("  - {} ({})", kb.name, kb.device);
        }
    }

    // Coerce to dyn MutableLookup at creation time.  The daemon-internal code
    // (watcher) can call mutation methods via MutableLookup.  A pointer cast
    // produces a dyn Lookup Arc for platform code, which only needs the
    // read-only interface.  Both Arcs share the same allocation.
    let state: Arc<RwLock<dyn MutableLookup>> =
        Arc::new(RwLock::new(keymapper::daemon::state::RuntimeState::new(
            initial_cache,
            all_keyboards,
        )));

    // Start hot-reloader thread
    let _watcher = keymapper::daemon::watcher::start_config_watcher(
        &config_path,
        Arc::clone(&state),
    )?;

    println!("Cross-platform runtime engines fully synchronized.");

    // Cast the Arc pointer to produce an Arc<RwLock<dyn Lookup>> that shares
    // the same underlying allocation.  Safe because dyn MutableLookup's vtable
    // is a superset of dyn Lookup's vtable, and the data pointer is identical.
    let platform_state: Arc<RwLock<dyn Lookup>> = unsafe {
        let ptr: *const RwLock<dyn MutableLookup> = Arc::as_ptr(&state);
        Arc::from_raw(ptr as *const RwLock<dyn Lookup>)
    };

    // On Linux, pass the filtered keyboard pairs for device-level capture.
    // On macOS and Windows, all keyboards are captured globally.
    #[cfg(target_os = "linux")]
    {
        // Select the already-opened devices that match the grab list.
        let grab_paths: std::collections::HashSet<&str> = keyboards_to_grab
            .iter()
            .map(|kb| kb.device.as_str())
            .collect();

        let opened_to_grab: Vec<_> = opened_devices
            .into_iter()
            .filter(|(info, _)| grab_paths.contains(info.device.as_str()))
            .collect();

        keymapper::platform::start_mapping(platform_state, opened_to_grab)
    }

    #[cfg(not(target_os = "linux"))]
    keymapper::platform::start_mapping(platform_state)
}
