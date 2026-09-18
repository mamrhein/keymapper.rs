// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::sync::Arc;

use keymapper::{
    common::{
        app_identity,
        config_path::find_config_path_strict,
        keyboard::{
            KeyboardInfo, KeyboardSpecifier, filter_keyboards_by_specifiers,
        },
    },
    daemon::{
        control, logging, mapping_cache::RuntimeLookupCache,
        state::RuntimeState, watcher::start_config_watcher,
    },
    platform::{list_keyboards, start_mapping},
};
use log::{error, info};
use parking_lot::RwLock;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    logging::init();

    // Open the control endpoint so the CLI can change the running daemon's
    // log level (and, later, other runtime settings) without a restart. This
    // runs as a background thread beside the mapping loop below; a bind
    // failure is logged and non-fatal.
    control::start();

    let config_path = find_config_path_strict().map_err(|e| {
        error!("Error: {e}");
        std::process::exit(1);
    })?;

    // Resolve to an absolute path so the watcher and cache compiler have
    // a stable reference regardless of later CWD changes.  Symlinks in
    // parent directory components are resolved here; the config file itself
    // was already verified to not be a symlink.
    let config_path = config_path.canonicalize().unwrap_or(config_path);

    let initial_cache = RuntimeLookupCache::compile_from_path(&config_path)?;

    // Discover connected keyboards to populate the device registry.  The
    // registry gets ALL keyboards so that filtering at rule-level can resolve
    // any device.
    //
    // On Linux this performs a udev scan, and `start_mapping` performs a
    // second scan for capture, so each device is opened twice at startup
    // (the first fd is dropped immediately).  That is a startup-only cost of
    // a few milliseconds, accepted in exchange for a uniform platform
    // signature — threading the already-opened devices through would require
    // a platform-specific `start_mapping`.
    let all_keyboards = list_keyboards().unwrap_or_default();

    // Determine which keyboards to actually grab based on the global filter.
    // Only matching keyboards are captured; others work normally.
    //
    // Clone the global filter before the cache is moved into RuntimeState,
    // since we also need it for the hot-plug monitor.
    let global_filter: Option<Vec<KeyboardSpecifier>> =
        initial_cache.global_keyboards().cloned();
    let keyboards_to_grab: Vec<KeyboardInfo> = filter_keyboards_by_specifiers(
        &all_keyboards,
        global_filter.as_deref(),
    );

    if !keyboards_to_grab.is_empty() {
        info!(
            "Grabbing {} keyboard(s) ({} total discovered):",
            keyboards_to_grab.len(),
            all_keyboards.len()
        );
        for kb in &keyboards_to_grab {
            info!("  - {} ({})", kb.name, kb.device);
        }
    }

    // The active-app source queries the platform directly; the state struct
    // keeps it as a closure so tests can supply a fixed value.
    let active_app_source: Box<dyn Fn() -> String + Send + Sync> =
        Box::new(app_identity::get_active_app_name);

    // Keep the concrete type so both trait objects below are produced by a
    // safe, compiler-checked unsized coercion (MutableLookup: Lookup).  Both
    // Arcs share the same allocation.
    let state = Arc::new(RwLock::new(RuntimeState::new(
        initial_cache,
        all_keyboards,
        active_app_source,
    )));

    // Start hot-reloader thread.  The watcher needs the mutable interface to
    // swap in recompiled caches; the concrete Arc is coerced to
    // `dyn MutableLookup` at the call site.
    let watcher_state = Arc::clone(&state);
    let _watcher = start_config_watcher(&config_path, watcher_state)?;

    info!("Cross-platform runtime engines fully synchronized.");

    // The platform layer only needs the read-only interface; the concrete Arc
    // is coerced to `dyn Lookup` at the call site.
    let platform_state = Arc::clone(&state);

    start_mapping(platform_state, global_filter)
}
