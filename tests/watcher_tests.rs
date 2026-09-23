// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Integration tests for the config hot-reload watcher against the real
//! platform backend (inotify, FSEvents, ReadDirectoryChangesW).
//!
//! The regression these tests guard (SEC-07): an inode-pinned file watch
//! silently stopped firing after the first atomic save (write temp + rename),
//! so hot-reload died unnoticed.  The watcher now observes the parent
//! directory and filters events by file name.

use std::{
    fs,
    path::Path,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use keymapper::{
    common::hid_usage::HidUsage,
    daemon::{
        mapping_cache::RuntimeLookupCache,
        state::{Lookup, MutableLookup, RuntimeState},
        watcher::start_config_watcher,
    },
};
use parking_lot::RwLock;
use tempfile::TempDir;

/// Per-save budget for the watcher event plus the debounce interval to
/// settle; generous to keep the test robust on loaded CI machines.
const RELOAD_TIMEOUT: Duration = Duration::from_secs(10);

/// Simulate an editor's atomic save: write a sibling temp file, then rename
/// it onto the target path.
fn atomic_save(path: &Path, content: &str) {
    let tmp = path.with_extension("yaml.tmp");
    fs::write(&tmp, content).expect("failed to write temp file");
    fs::rename(&tmp, path).expect("failed to rename onto config path");
}

fn state_with(yaml: &str) -> Arc<RwLock<RuntimeState>> {
    let cache = RuntimeLookupCache::compile_from_str(yaml)
        .expect("failed to compile initial cache");
    Arc::new(RwLock::new(RuntimeState::new(
        cache,
        Vec::new(),
        Box::new(|| "test".to_string()),
    )))
}

/// Poll until the global lookup for key `A` resolves to `expected`, or the
/// reload budget is exhausted.
fn wait_for_output(state: &RwLock<RuntimeState>, expected: HidUsage) -> bool {
    let deadline = Instant::now() + RELOAD_TIMEOUT;
    loop {
        let hit =
            state
                .read()
                .global(HidUsage::A, 0, None)
                .is_some_and(|keys| {
                    keys.first().is_some_and(|k| k.usage == expected)
                });
        if hit {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn hot_reload_survives_repeated_atomic_saves() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let config_path = dir.path().join("config.yaml");

    atomic_save(&config_path, "- mappings:\n    A: CapsLock\n");
    let state = state_with("- mappings:\n    A: CapsLock\n");

    let watcher_state: Arc<RwLock<dyn MutableLookup>> = state.clone();
    let _watcher = start_config_watcher(&config_path, watcher_state)
        .expect("failed to start config watcher");

    // First atomic save: even the old inode-based watch picked this one up.
    atomic_save(&config_path, "- mappings:\n    A: LeftShift\n");
    assert!(
        wait_for_output(&state, HidUsage::LeftShift),
        "first atomic save did not hot-reload the config"
    );

    // Second atomic save: with the old inode-pinned watch this event never
    // reached the daemon, leaving the stale mapping in place forever.
    atomic_save(&config_path, "- mappings:\n    A: LeftControl\n");
    assert!(
        wait_for_output(&state, HidUsage::LeftControl),
        "second atomic save did not hot-reload the config (SEC-07 regression)"
    );
}

#[test]
fn in_place_write_triggers_reload() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let config_path = dir.path().join("config.yaml");

    fs::write(&config_path, "- mappings:\n    A: CapsLock\n")
        .expect("failed to write config");
    let state = state_with("- mappings:\n    A: CapsLock\n");

    let watcher_state: Arc<RwLock<dyn MutableLookup>> = state.clone();
    let _watcher = start_config_watcher(&config_path, watcher_state)
        .expect("failed to start config watcher");

    // A plain truncate-and-write save (no rename) must also reload.
    fs::write(&config_path, "- mappings:\n    A: LeftShift\n")
        .expect("failed to rewrite config");
    assert!(
        wait_for_output(&state, HidUsage::LeftShift),
        "in-place write did not hot-reload the config"
    );
}

#[test]
fn delete_then_recreate_triggers_reload() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let config_path = dir.path().join("config.yaml");

    fs::write(&config_path, "- mappings:\n    A: CapsLock\n")
        .expect("failed to write config");
    let state = state_with("- mappings:\n    A: CapsLock\n");

    let watcher_state: Arc<RwLock<dyn MutableLookup>> = state.clone();
    let _watcher = start_config_watcher(&config_path, watcher_state)
        .expect("failed to start config watcher");

    // Some editors unlink the file before writing the new version.  The
    // removal alone must not poison the watch; the recreate reloads.
    fs::remove_file(&config_path).expect("failed to remove config");
    thread::sleep(Duration::from_millis(100));
    fs::write(&config_path, "- mappings:\n    A: LeftShift\n")
        .expect("failed to recreate config");

    assert!(
        wait_for_output(&state, HidUsage::LeftShift),
        "delete-then-recreate did not hot-reload the config"
    );
}

#[test]
fn sibling_file_changes_do_not_disturb() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let config_path = dir.path().join("config.yaml");

    fs::write(&config_path, "- mappings:\n    A: CapsLock\n")
        .expect("failed to write config");
    let state = state_with("- mappings:\n    A: CapsLock\n");

    let watcher_state: Arc<RwLock<dyn MutableLookup>> = state.clone();
    let _watcher = start_config_watcher(&config_path, watcher_state)
        .expect("failed to start config watcher");

    // Churn in the watched directory that never touches the config name; the
    // mapping must stay at its initial value (no reload, no error).
    for i in 0..5 {
        fs::write(dir.path().join(format!("swap_{i}.yaml")), "junk")
            .expect("failed to write swap file");
    }
    thread::sleep(Duration::from_millis(800));
    assert!(
        state
            .read()
            .global(HidUsage::A, 0, None)
            .is_some_and(|keys| keys
                .first()
                .is_some_and(|k| k.usage == HidUsage::CapsLock)),
        "sibling file churn changed the active mapping"
    );

    // The config still hot-reloads after the sibling churn.
    atomic_save(&config_path, "- mappings:\n    A: LeftShift\n");
    assert!(
        wait_for_output(&state, HidUsage::LeftShift),
        "config did not hot-reload after sibling file churn"
    );
}
