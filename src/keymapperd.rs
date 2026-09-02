// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::sync::Arc;

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
    // On Linux this performs a udev scan, and `start_mapping` performs a
    // second scan for capture, so each device is opened twice at startup
    // (the first fd is dropped immediately).  That is a startup-only cost of
    // a few milliseconds, accepted in exchange for a uniform platform
    // signature — threading the already-opened devices through would require
    // a platform-specific `start_mapping`.
    let all_keyboards =
        keymapper::platform::list_keyboards().unwrap_or_default();

    // Determine which keyboards to actually grab based on the global filter.
    // Only matching keyboards are captured; others work normally.
    //
    // Clone the global filter before the cache is moved into RuntimeState,
    // since we also need it for the hot-plug monitor.
    let global_filter: Option<
        Vec<keymapper::common::keyboard::KeyboardSpecifier>,
    > = initial_cache.global_keyboards().cloned();
    let keyboards_to_grab: Vec<keymapper::common::keyboard::KeyboardInfo> =
        keymapper::common::keyboard::filter_keyboards_by_specifiers(
            &all_keyboards,
            global_filter.as_deref(),
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

    // Keep the concrete type so both trait objects below are produced by a
    // safe, compiler-checked unsized coercion (MutableLookup: Lookup).  Both
    // Arcs share the same allocation.
    let state = Arc::new(RwLock::new(keymapper::daemon::state::RuntimeState::new(
        initial_cache,
        all_keyboards,
        // Inject the active-app source.  It honors the e2e override and
        // falls back to the platform query, so production runs pay nothing
        // for it and the state struct stays free of test-specific code.
        Box::new(keymapper::daemon::test_hooks::active_app_name),
    )));

    // Start hot-reloader thread.  The watcher needs the mutable interface to
    // swap in recompiled caches; the concrete Arc is coerced to
    // `dyn MutableLookup` at the call site.
    let watcher_state = Arc::clone(&state);
    let _watcher = keymapper::daemon::watcher::start_config_watcher(
        &config_path,
        watcher_state,
    )?;

    println!("Cross-platform runtime engines fully synchronized.");

    // The platform layer only needs the read-only interface; the concrete Arc
    // is coerced to `dyn Lookup` at the call site.
    let platform_state = Arc::clone(&state);

    // Inject the e2e readiness hook.  It is a no-op unless the harness set
    // `KEYMAPPER_READY_FILE`, so production runs pay nothing for it, and the
    // platform layer stays free of test-specific side effects.
    keymapper::platform::start_mapping(
        platform_state,
        global_filter,
        Some(Box::new(keymapper::daemon::test_hooks::signal_ready)),
    )
}
