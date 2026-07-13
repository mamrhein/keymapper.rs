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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(config_path) = keymapperd::config_path::find_config_path() else {
        keymapperd::config_path::print_search_locations();
        std::process::exit(1);
    };

    // Resolve to an absolute path so the watcher and cache compiler have
    // a stable reference regardless of later CWD changes.
    let config_path = config_path.canonicalize().unwrap_or(config_path);

    let initial_cache =
        keymapperd::mapping_cache::RuntimeLookupCache::compile_from_path(
            &config_path,
        )?;

    // Coerce to dyn Lookup at creation time.  All Arc::clone calls
    // downstream inherit this trait-object type, so platform modules
    // never see the concrete RuntimeState shape.
    let state: Arc<RwLock<dyn keymapperd::state::Lookup>> =
        Arc::new(RwLock::new(keymapperd::state::RuntimeState::new(
            initial_cache,
            String::from("unknown"),
        )));

    // Start hot-reloader thread
    let _watcher = keymapperd::watcher::start_config_watcher(
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

            // Read-check -> conditional write-escalation.  Uses trait
            // methods so this code is also decoupled from RuntimeState.
            if !tracker_state.read().active_app().eq(&current_focused_app) {
                let mut write_guard = tracker_state.write();
                if !write_guard.active_app().eq(&current_focused_app) {
                    write_guard.set_active_app(current_focused_app);
                }
            }

            thread::sleep(Duration::from_millis(100));
        }
    });

    println!("Cross-platform runtime engines fully synchronized.");

    keymapperd::os::start_mapping(Arc::clone(&state))
}
