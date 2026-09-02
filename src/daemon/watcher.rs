// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use parking_lot::RwLock;

use super::{
    config_io::read_config_content, mapping_cache::RuntimeLookupCache,
    state::MutableLookup,
};

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
    state: Arc<RwLock<dyn MutableLookup>>,
) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let mut consecutive_errors: usize = 0;
        let mut last_log: Option<Instant> = None;

        loop {
            // Block until a filesystem event arrives.  Only real events may
            // trigger a reload — the previous design treated every debounce
            // timeout as a quiet period, which reloaded an unmodified config
            // every DEBOUNCE_INTERVAL.
            if rx.recv().is_err() {
                // The watcher was dropped; exit the thread.
                break;
            }

            // Debounce: keep consuming events until the file system goes
            // quiet for DEBOUNCE_INTERVAL after the last event.  Editors
            // that write atomically (write-to-temp + rename) emit several
            // events per save; this coalesces them into one reload.
            loop {
                match rx.recv_timeout(DEBOUNCE_INTERVAL) {
                    Ok(()) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
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

/// Attempt a single reload of the configuration file.  The file is read via
/// [`read_config_content`], which applies the same security checks as the
/// initial load (symlink, regular-file, size, ownership, world-writable) on a
/// single open descriptor.  On success the compiled cache is swapped in.
fn attempt_reload(
    config_path: &Path,
    state: &Arc<RwLock<dyn MutableLookup>>,
) -> ReloadResult {
    let content = match read_config_content(config_path) {
        Ok(content) => content,
        Err(err) => return ReloadResult::Err(err.to_string()),
    };

    reload_from_str(&content, state)
}

/// Parse and compile the config string, then swap the runtime cache.
fn reload_from_str(
    content: &str,
    state: &Arc<RwLock<dyn MutableLookup>>,
) -> ReloadResult {
    let new_cache = match RuntimeLookupCache::compile_from_str(content) {
        Ok(cache) => cache,
        Err(err) => {
            return ReloadResult::Err(err.to_string());
        }
    };

    // Swap the cache inside the write lock, then release the lock before
    // printing the success message.  This ordering guarantees that by the
    // time an observer sees the message in stdout, the new cache is already
    // visible to all readers of the RwLock.
    {
        let mut write_guard = state.write();
        write_guard.set_lookup_cache(new_cache);
    }

    // Flush stdout to ensure the message reaches any pipe consumers before
    // returning.  When stdout is block-buffered (e.g., when captured by a
    // subprocess), println! alone may leave the message in an internal buffer.
    println!("Configuration hot-swapped successfully!");
    let _ = std::io::stdout().flush();

    ReloadResult::Ok
}

pub fn start_config_watcher<P: AsRef<Path>>(
    config_path: P,
    state: Arc<RwLock<dyn MutableLookup>>,
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
