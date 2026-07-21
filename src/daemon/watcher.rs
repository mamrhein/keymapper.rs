// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use parking_lot::RwLock;

use super::{mapping_cache::RuntimeLookupCache, state::Lookup};

/// Maximum config file size in bytes (1 MB).  A key-mapping configuration
/// should never approach this limit; a larger file indicates either a write
/// gone wrong or an adversarial payload.
const MAX_CONFIG_SIZE: u64 = 1024 * 1024;

/// Debounce interval: wait this long after the last filesystem event before
/// attempting a reload.  Editors that write atomically (write-to-temp +
/// rename) can emit multiple events; this coalesces them.
const DEBOUNCE_INTERVAL: Duration = Duration::from_millis(500);

/// Error log throttle: after this many consecutive reload failures, suppress
/// further error output until a successful reload resets the counter.
const ERROR_THROTTLE_LIMIT: usize = 5;

/// Result of a single hot-reload attempt.
enum ReloadResult {
    /// Config was successfully loaded and the cache was swapped.
    Ok,
    /// Reload failed; message is logged only when throttling permits it.
    Err(String),
}

/// Spawn a background reload thread and return the sender for the notify
/// closure to use.  The watcher itself is configured as usual; the closure
/// only pushes events onto a channel.
fn spawn_reload_thread(
    path_to_watch: Arc<PathBuf>,
    state: Arc<RwLock<dyn Lookup>>,
) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let mut consecutive_errors: usize = 0;
        let mut last_log: Option<Instant> = None;

        loop {
            // Wait for an event, but timeout after DEBOUNCE_INTERVAL.
            // If we receive more events during that window they queue up;
            // once the channel is empty after a quiet period we proceed.
            match rx.recv_timeout(DEBOUNCE_INTERVAL) {
                Ok(()) => {
                    // An event arrived — consume any others that queued up
                    // during the debounce window.
                    drain_channel(&rx);
                    // Reset the debounce: wait for a quiet period.
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Quiet period elapsed — proceed with reload.
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // The watcher was dropped; exit the thread.
                    break;
                }
            }

            match attempt_reload(&path_to_watch, &state) {
                ReloadResult::Ok => {
                    consecutive_errors = 0;
                    last_log = None;
                }
                ReloadResult::Err(msg) => {
                    consecutive_errors += 1;

                    // Throttle error output: log at most once per
                    // ERROR_THROTTLE_LIMIT failures, with increasing gaps.
                    let should_log = if consecutive_errors
                        <= ERROR_THROTTLE_LIMIT
                    {
                        true
                    } else {
                        // After the throttle limit, log only if enough time
                        // has passed since the last message.  This prevents
                        // log flooding from a persistently invalid config.
                        !matches!(
                            last_log,
                            Some(ts) if ts.elapsed() < Duration::from_secs(30),
                        )
                    };

                    if should_log {
                        eprintln!(
                            "Failed to hot-reload configuration: {}",
                            msg
                        );
                        if consecutive_errors > ERROR_THROTTLE_LIMIT {
                            eprintln!(
                                "(Throttling further error output until a \
                                 successful reload.)"
                            );
                        }
                        last_log = Some(Instant::now());
                    }
                }
            }
        }
    });

    tx
}

/// Drain any pending messages from the channel so we only reload once per
/// burst of filesystem events.
fn drain_channel(rx: &mpsc::Receiver<()>) {
    while rx.try_recv().is_ok() {}
}

/// Attempt a single reload of the configuration file, performing security
/// checks before parsing.
fn attempt_reload(
    config_path: &Path,
    state: &Arc<RwLock<dyn Lookup>>,
) -> ReloadResult {
    // Security check: file still exists and is a regular file.
    let Ok(metadata) = config_path.metadata() else {
        return ReloadResult::Err("config file not found".to_string());
    };

    if !metadata.is_file() {
        return ReloadResult::Err(
            "config path is not a regular file".to_string(),
        );
    }

    // Security check: file size is within acceptable bounds.
    if metadata.len() > MAX_CONFIG_SIZE {
        return ReloadResult::Err(format!(
            "config file is too large ({} bytes, limit {})",
            metadata.len(),
            MAX_CONFIG_SIZE,
        ));
    }

    // Security check: on Unix, verify the file is owned by the current user.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = metadata.uid();
        if uid != unsafe { libc::getuid() } {
            return ReloadResult::Err(format!(
                "config file is owned by uid {} (current user: {})",
                uid,
                unsafe { libc::getuid() },
            ));
        }

        // Security check: file is not world-writable (prevents other users
        // on the same system from tampering with it).
        let mode = metadata.mode() as libc::mode_t;
        if (mode & libc::S_IWOTH) != 0 {
            return ReloadResult::Err(
                "config file is world-writable".to_string(),
            );
        }
    }

    // Reparse and recompile via the shared loader.
    match RuntimeLookupCache::compile_from_path(config_path) {
        Ok(new_cache) => {
            // Safely acquire a write lock and swap out the cache via the
            // trait interface.
            let mut write_guard = state.write();
            write_guard.set_lookup_cache(new_cache);
            println!("Configuration hot-swapped successfully!");
        }
        Err(err) => {
            // Parsing failed; keep the previous configuration rules safe.
            return ReloadResult::Err(err.to_string());
        }
    }

    ReloadResult::Ok
}

pub fn start_config_watcher<P: AsRef<Path>>(
    config_path: P,
    state: Arc<RwLock<dyn Lookup>>,
) -> Result<RecommendedWatcher, notify::Error> {
    let path_to_watch = Arc::new(config_path.as_ref().to_owned());
    let reload_tx = spawn_reload_thread(Arc::clone(&path_to_watch), state);

    // Create a cross-platform watcher infrastructure.  The closure only
    // sends reload requests; the background thread performs debouncing
    // and the actual reload.
    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| match result {
            Ok(event) => {
                // We only care about file modifications (e.g., user hits save
                // in text editor).
                if let EventKind::Modify(_) = event.kind {
                    // Notify the background thread.  If the channel is full or
                    // disconnected, silently drop — the next event will retry.
                    let _ = reload_tx.send(());
                }
            }
            Err(e) => eprintln!("File system watcher error: {:?}", e),
        },
        Config::default(),
    )?;

    watcher.watch(config_path.as_ref(), RecursiveMode::NonRecursive)?;

    Ok(watcher)
}
