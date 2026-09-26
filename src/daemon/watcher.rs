// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use log::{error, info, warn};
use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::ModifyKind,
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
                        error!("Failed to hot-reload configuration: {msg}");
                        if consecutive_errors > ERROR_THROTTLE_LIMIT {
                            error!(
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
/// single open descriptor and re-inspects the parent-directory chain, so a
/// symlink swapped into a parent directory after startup aborts this reload.
/// On success the compiled cache is swapped in.
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
    // logging the success message.  This ordering guarantees that by the
    // time the message is logged, the new cache is already visible to all
    // readers of the RwLock.
    {
        let mut write_guard = state.write();
        write_guard.set_lookup_cache(new_cache);
    }

    info!("Configuration hot-swapped successfully!");

    ReloadResult::Ok
}

/// How the watcher should react to a filesystem event.
#[derive(Debug, PartialEq, Eq)]
enum Interest {
    /// The config file was created or modified; schedule a (re)load.
    Reload,
    /// The config file was unlinked or renamed away; a later event must
    /// recreate it before a reload can succeed.
    ConfigRemoved,
    /// The watched directory itself was removed or renamed, which kills the
    /// underlying OS watch until the watcher is re-armed.
    WatchDirRemoved,
    /// Irrelevant for hot-reload (e.g., an event for a sibling file).
    Ignore,
}

/// Classify a directory-watch event with respect to the watched config file.
///
/// `watch_dir` is the directory handed to [`Watcher::watch`], so event paths
/// are either `watch_dir` itself or `watch_dir.join(name)` for a child.  The
/// config file is identified by name because a rename onto the path carries
/// the config name in at least one of its event paths.
fn classify_event(
    event: &Event,
    watch_dir: &Path,
    config_name: &OsStr,
) -> Interest {
    let mut watch_dir_gone = false;

    for path in &event.paths {
        if path == watch_dir {
            // Removal or rename of the watched directory detaches the OS
            // watch; other notifications about the directory are irrelevant.
            if matches!(
                event.kind,
                EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
            ) {
                watch_dir_gone = true;
            }
        } else if path.file_name() == Some(config_name) {
            return match event.kind {
                // Covers in-place writes (`Modify(Data)`), atomic saves
                // renamed onto the path (`Modify(Name(To))` on Linux and
                // Windows), and delete-then-write editors (`Create`).
                EventKind::Modify(_) | EventKind::Create(_) => {
                    Interest::Reload
                }
                EventKind::Remove(_) => Interest::ConfigRemoved,
                _ => Interest::Ignore,
            };
        }
    }

    if watch_dir_gone {
        Interest::WatchDirRemoved
    } else {
        Interest::Ignore
    }
}

pub fn start_config_watcher<P: AsRef<Path>>(
    config_path: P,
    state: Arc<RwLock<dyn MutableLookup>>,
) -> Result<RecommendedWatcher, notify::Error> {
    let config_path = config_path.as_ref().to_owned();
    let config_name = config_path
        .file_name()
        .map(OsStr::to_owned)
        .ok_or_else(|| {
            notify::Error::new(notify::ErrorKind::Generic(format!(
                "config path {} has no file name",
                config_path.display()
            )))
        })?;
    // Watch the parent *directory* instead of the file itself: under inotify
    // a file watch pins the file's inode, so an atomic save (write temp +
    // rename) leaves the watch on the unlinked inode and every subsequent
    // save goes unnoticed.  A non-recursive directory watch survives renames
    // on all three backends; events for siblings are filtered out by name.
    let watch_dir = match config_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_owned(),
        // Bare relative path like "config.yaml" — watch the working directory.
        _ => PathBuf::from("."),
    };

    let path_to_watch = Arc::new(config_path);
    let reload_tx = spawn_reload_thread(Arc::clone(&path_to_watch), state);
    let closure_dir = watch_dir.clone();

    // Create a cross-platform watcher infrastructure.  The closure only
    // sends reload requests; the background thread performs debouncing
    // and the actual reload.
    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| match result {
            Ok(event) => {
                match classify_event(&event, &closure_dir, &config_name) {
                    Interest::Reload => {
                        // Notify the background thread.  If the channel is
                        // full or disconnected, silently drop — the next
                        // event will retry.
                        let _ = reload_tx.send(());
                    }
                    Interest::ConfigRemoved => warn!(
                        "Watched config file {} was removed; waiting for it \
                         to be recreated.",
                        path_to_watch.display()
                    ),
                    Interest::WatchDirRemoved => warn!(
                        "Watched config directory {} was removed or renamed; \
                         hot-reload is no longer active.",
                        closure_dir.display()
                    ),
                    Interest::Ignore => {}
                }
            }
            Err(e) => error!("File system watcher error: {e:?}"),
        },
        Config::default(),
    )?;

    watcher.watch(&watch_dir, RecursiveMode::NonRecursive)?;

    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use notify::event::{
        AccessKind, CreateKind, DataChange, RemoveKind, RenameMode,
    };

    use super::*;

    fn dir() -> PathBuf {
        PathBuf::from("/etc/keymapper")
    }

    fn config_path() -> PathBuf {
        dir().join("config.yaml")
    }

    fn event(kind: EventKind, paths: &[&Path]) -> Event {
        let mut event = Event::new(kind);
        for path in paths {
            event = event.add_path(path.to_path_buf());
        }
        event
    }

    #[test]
    fn in_place_modify_triggers_reload() {
        let ev = event(
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            &[config_path().as_path()],
        );
        assert_eq!(
            classify_event(&ev, &dir(), OsStr::new("config.yaml")),
            Interest::Reload
        );
    }

    #[test]
    fn rename_onto_path_triggers_reload() {
        // What an atomic save emits on Linux (MOVED_TO) and Windows
        // (FILE_ACTION_RENAMED_NEW_NAME) for the config path.
        let ev = event(
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            &[config_path().as_path()],
        );
        assert_eq!(
            classify_event(&ev, &dir(), OsStr::new("config.yaml")),
            Interest::Reload
        );
    }

    #[test]
    fn paired_rename_triggers_reload_via_dest_path() {
        // A `RenameMode::Both` event carries source and destination; only
        // the destination names the config file.
        let ev = event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[
                dir().join(".config.yaml.tmp").as_path(),
                config_path().as_path(),
            ],
        );
        assert_eq!(
            classify_event(&ev, &dir(), OsStr::new("config.yaml")),
            Interest::Reload
        );
    }

    #[test]
    fn create_triggers_reload() {
        // Delete-then-write editors recreate the file in a second event.
        let ev = event(
            EventKind::Create(CreateKind::File),
            &[config_path().as_path()],
        );
        assert_eq!(
            classify_event(&ev, &dir(), OsStr::new("config.yaml")),
            Interest::Reload
        );
    }

    #[test]
    fn remove_waits_for_recreation() {
        let ev = event(
            EventKind::Remove(RemoveKind::File),
            &[config_path().as_path()],
        );
        assert_eq!(
            classify_event(&ev, &dir(), OsStr::new("config.yaml")),
            Interest::ConfigRemoved
        );
    }

    #[test]
    fn rename_away_of_other_file_is_ignored() {
        // During an atomic save the temp file's MOVED_FROM / rename-from
        // event carries only the temp name.
        let ev = event(
            EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            &[dir().join(".config.yaml.tmp").as_path()],
        );
        assert_eq!(
            classify_event(&ev, &dir(), OsStr::new("config.yaml")),
            Interest::Ignore
        );
    }

    #[test]
    fn sibling_modify_is_ignored() {
        let ev = event(
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            &[dir().join("other.yaml").as_path()],
        );
        assert_eq!(
            classify_event(&ev, &dir(), OsStr::new("config.yaml")),
            Interest::Ignore
        );
    }

    #[test]
    fn access_event_is_ignored() {
        let ev = event(
            EventKind::Access(AccessKind::Read),
            &[config_path().as_path()],
        );
        assert_eq!(
            classify_event(&ev, &dir(), OsStr::new("config.yaml")),
            Interest::Ignore
        );
    }

    #[test]
    fn watched_dir_removal_is_reported() {
        let ev =
            event(EventKind::Remove(RemoveKind::Folder), &[dir().as_path()]);
        assert_eq!(
            classify_event(&ev, &dir(), OsStr::new("config.yaml")),
            Interest::WatchDirRemoved
        );
    }
}
