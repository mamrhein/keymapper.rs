// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::sync::Arc;

use clap::Parser;
use parking_lot::RwLock;

// Import Lookup so read-only trait methods are in scope, and MutableLookup so
// mutation methods are callable on the concrete RuntimeState type.
use keymapper::daemon::state::{Lookup, MutableLookup};

/// Cross-platform key-remapping daemon.
#[derive(Parser)]
struct Args {
    /// Override the input device path (Linux only).
    ///
    /// When specified, the daemon captures keyboard events exclusively from
    /// this device node instead of auto-discovering connected keyboards.
    /// Primarily used for end-to-end testing with virtual keyboards.
    #[arg(long)]
    device: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

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

    // Discover connected keyboards to populate the device registry.
    // Used for keyboard filtering at runtime.
    let keyboards = keymapper::platform::list_keyboards().unwrap_or_default();

    // Coerce to dyn MutableLookup at creation time.  The daemon-internal code
    // (watcher) can call mutation methods via MutableLookup.  A pointer cast
    // produces a dyn Lookup Arc for platform code, which only needs the
    // read-only interface.  Both Arcs share the same allocation.
    let state: Arc<RwLock<dyn MutableLookup>> = Arc::new(RwLock::new(
        keymapper::daemon::state::RuntimeState::new(initial_cache, keyboards),
    ));

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

    keymapper::platform::start_mapping(platform_state, args.device.as_deref())
}
