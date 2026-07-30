// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{sync::Arc, thread, time::Duration};

use parking_lot::RwLock;

// Import Lookup so read-only trait methods are in scope, and MutableLookup so
// mutation methods are callable on the concrete RuntimeState type.
use keymapper::daemon::state::{Lookup, MutableLookup};

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

    // Coerce to dyn MutableLookup at creation time.  The daemon-internal code
    // (watcher, tracker) can call mutation methods via MutableLookup.  A
    // pointer cast produces a dyn Lookup Arc for platform code, which only
    // needs the read-only interface.  Both Arcs share the same allocation.
    let state: Arc<RwLock<dyn MutableLookup>> =
        Arc::new(RwLock::new(keymapper::daemon::state::RuntimeState::new(
            initial_cache,
            String::from("unknown"),
        )));

    // Start hot-reloader thread
    let _watcher = keymapper::daemon::watcher::start_config_watcher(
        &config_path,
        Arc::clone(&state),
    )?;

    // Start tracking foreground windows natively
    let tracker_state = Arc::clone(&state);
    thread::spawn(move || {
        println!("Native window tracking thread active.");
        loop {
            let current_focused_app =
                match active_win_pos_rs::get_active_window() {
                    Ok(window) => window.app_name,
                    Err(_) => String::from("unknown"),
                };

            // Read-check -> conditional write-escalation.
            if !current_focused_app.eq(&**tracker_state.read().active_app()) {
                let mut write_guard = tracker_state.write();
                if !current_focused_app.eq(&**write_guard.active_app()) {
                    write_guard.set_active_app(current_focused_app);
                }
            }

            thread::sleep(Duration::from_millis(100));
        }
    });

    println!("Cross-platform runtime engines fully synchronized.");

    // Cast the Arc pointer to produce an Arc<RwLock<dyn Lookup>> that shares
    // the same underlying allocation.  Safe because dyn MutableLookup's vtable
    // is a superset of dyn Lookup's vtable, and the data pointer is identical.
    let platform_state: Arc<RwLock<dyn Lookup>> = unsafe {
        let ptr: *const RwLock<dyn MutableLookup> = Arc::as_ptr(&state);
        Arc::from_raw(ptr as *const RwLock<dyn Lookup>)
    };

    keymapper::platform::start_mapping(platform_state)
}
