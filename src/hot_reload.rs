// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{fs, path::Path, sync::Arc};

use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use parking_lot::RwLock;

use crate::{
    RuntimeState, config::AppConfig, mapping_cache::RuntimeLookupCache,
};

pub fn start_config_watcher<P: AsRef<Path>>(
    config_path: P,
    state: Arc<RwLock<RuntimeState>>,
) -> Result<RecommendedWatcher, notify::Error> {
    let path_to_watch = config_path.as_ref().to_owned();

    // 1. Create a cross-platform watcher infrastructure.
    // The closure triggers whenever an OS file system signal fires.
    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| match result {
            Ok(event) => {
                // We only care about file modifications (e.g., user hits save
                // in text editor)
                if let EventKind::Modify(_) = event.kind {
                    println!(
                        "Configuration file modification detected. \
                         Reloading..."
                    );

                    // Attempt to safely reparse and recompile the file
                    match reload_and_compile_cache(&path_to_watch) {
                        Ok(new_cache) => {
                            // Safely acquire a write lock and swap out the
                            // cache
                            let mut write_guard = state.write();
                            write_guard.lookup_cache = new_cache;
                            println!(
                                "Configuration hot-swapped successfully!"
                            );
                        }
                        Err(err) => {
                            eprintln!(
                                "Failed to hot-reload configuration: {}",
                                err
                            );
                            eprintln!(
                                "Keeping previous configuration rules safe \
                                 to prevent crashes."
                            );
                        }
                    }
                }
            }
            Err(e) => eprintln!("File system watcher error error: {:?}", e),
        },
        Config::default(),
    )?;

    // 2. Point the native OS event system to your configuration file
    watcher.watch(config_path.as_ref(), RecursiveMode::NonRecursive)?;

    Ok(watcher)
}

/// Helper function to perform isolated file reading and compilation
fn reload_and_compile_cache(
    path: &Path,
) -> Result<RuntimeLookupCache, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let parsed_config = AppConfig::load_from_str(&content)?;
    let compiled_cache =
        RuntimeLookupCache::compile_from_config(&parsed_config);
    Ok(compiled_cache)
}
