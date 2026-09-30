// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Short-TTL cache with single-flight refresh.
//!
//! [`TtlValue`] caches a single value for a fixed time-to-live and refreshes
//! it through a caller-supplied source closure.  The refresh is
//! *single-flight*: the first caller to find the entry expired claims the
//! refresh slot and runs the (potentially blocking) source, while every
//! other caller immediately receives the last known value instead of queueing
//! behind a stalled refresh.  An expired entry therefore causes at worst a
//! brief staleness for all callers, never a lock-up of a hot path.

use std::{
    fmt,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

/// A single cached value with a short time-to-live and single-flight
/// refresh.
pub(crate) struct TtlValue<T> {
    /// How long a published value stays fresh.
    ttl: Duration,
    /// The current value together with its freshness deadline.  The lock is
    /// only ever held for the duration of a (cheap) read or write, never
    /// across the refresh itself.
    entry: Mutex<TtlEntry<T>>,
    /// Single-flight guard for the refresh.  The thread that finds the entry
    /// expired holds this lock for the duration of the (blocking) refresh;
    /// concurrent callers serve the stale entry instead of queueing behind a
    /// stalled refresh.  Without it, one slow source would block every
    /// caller on `entry`.
    refresh: Mutex<()>,
}

struct TtlEntry<T> {
    value: T,
    /// Point in time at which [`TtlValue::get_with`] starts treating the
    /// value as stale.
    expires_at: Instant,
}

impl<T: Clone> TtlValue<T> {
    /// Create a cache seeded with `initial`, fresh for `ttl`.
    pub(crate) fn new(ttl: Duration, initial: T) -> Self {
        Self {
            ttl,
            entry: Mutex::new(TtlEntry {
                value: initial,
                expires_at: Instant::now() + ttl,
            }),
            refresh: Mutex::new(()),
        }
    }

    /// The current value, refreshing it through `source` when the cached
    /// entry has expired.
    ///
    /// `source` runs _outside_ the value lock and only in the one caller
    /// that claims the refresh slot; every other caller receives the stale
    /// value immediately.  If `source` panics, the slot guard's `Drop`
    /// releases the refresh slot, so the cache cannot be wedged permanently.
    pub(crate) fn get_with(&self, source: impl FnOnce() -> T) -> T {
        {
            let entry = self.entry.lock();
            if Instant::now() < entry.expires_at {
                return entry.value.clone();
            }
        }
        // The entry is stale.  Serve it while a single caller refreshes.
        let Some(_refresh_slot) = self.refresh.try_lock() else {
            return self.entry.lock().value.clone();
        };
        let value = source();
        let mut entry = self.entry.lock();
        entry.value = value.clone();
        entry.expires_at = Instant::now() + self.ttl;
        value
    }

    /// Mark the entry stale so the next [`get_with`](Self::get_with) forces
    /// a refresh, without waiting for the TTL to elapse.
    #[cfg(test)]
    pub(crate) fn invalidate(&self) {
        // Setting the deadline to "now" makes the freshness check in
        // `get_with` fail on the very next call.
        self.entry.lock().expires_at = Instant::now();
    }
}

impl<T: Clone + fmt::Debug> fmt::Debug for TtlValue<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entry = self.entry.lock();
        f.debug_struct("TtlValue")
            .field("ttl", &self.ttl)
            .field("value", &entry.value)
            .field("fresh", &(Instant::now() < entry.expires_at))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };

    use super::*;

    /// TTL used by the tests below; long enough that "fresh" assertions are
    /// not scheduling-sensitive, short enough that the stalled-refresh
    /// timing bound stays meaningful.
    const TTL: Duration = Duration::from_millis(100);

    #[test]
    fn fresh_value_is_served_without_refresh() {
        let calls = Arc::new(AtomicUsize::new(0));
        let src_calls = Arc::clone(&calls);
        let value = TtlValue::<Arc<str>>::new(TTL, Arc::from("seed"));

        // The entry seeded by `new` is fresh, so no refresh runs.
        let served = value.get_with(|| {
            src_calls.fetch_add(1, Ordering::SeqCst);
            Arc::from("refreshed")
        });
        assert_eq!(&*served, "seed");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn expired_entry_refreshes_once_and_publishes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let src_calls = Arc::clone(&calls);
        let value = TtlValue::<Arc<str>>::new(TTL, Arc::from("seed"));
        value.invalidate();

        let refresh = move || {
            let n = src_calls.fetch_add(1, Ordering::SeqCst);
            Arc::from(format!("app{n}"))
        };
        assert_eq!(&*value.get_with(refresh), "app0");
        // Serving the refreshed entry within the TTL must not re-refresh.
        assert_eq!(
            &*value.get_with(|| unreachable!("re-refresh within TTL")),
            "app0"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A stalled refresh must not block concurrent callers: exactly one
    /// thread runs the source (outside the value lock) while the others
    /// immediately serve the stale entry.
    #[test]
    fn stalled_refresh_does_not_block_callers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let src_calls = Arc::clone(&calls);
        // Signals set by the source when a refresh starts and by the test
        // when the stalled refresh may finish.
        let query_started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let src_started = Arc::clone(&query_started);
        let src_release = Arc::clone(&release);

        let value =
            Arc::new(TtlValue::<Arc<str>>::new(TTL, Arc::from("seed")));
        value.invalidate();

        let value_refresher = Arc::clone(&value);
        std::thread::scope(|s| {
            let refresher = s.spawn(move || {
                value_refresher.get_with(move || {
                    src_calls.fetch_add(1, Ordering::SeqCst);
                    src_started.store(true, Ordering::SeqCst);
                    // Simulate an IPC peer that stops responding.
                    while !src_release.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Arc::from("fresh")
                })
            });

            // Wait until the refresh is genuinely in flight, i.e. the
            // source has been entered (which happens while the refresh
            // slot, but not the value lock, is held).
            while !query_started.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }

            // Concurrent callers must serve the stale value immediately
            // rather than queueing behind the stalled refresh, and must
            // not run a second source themselves.
            let start = Instant::now();
            let served = value
                .get_with(|| unreachable!("concurrent caller ran a refresh"));
            let elapsed = start.elapsed();
            assert_eq!(&*served, "seed");
            assert!(
                elapsed < TTL,
                "caller blocked behind the stalled refresh ({elapsed:?})",
            );

            // Let the stalled refresh finish, then verify its result got
            // published.
            release.store(true, Ordering::SeqCst);
            assert_eq!(&*refresher.join().unwrap(), "fresh");
        });
        assert_eq!(
            &*value.get_with(|| unreachable!("re-refresh within TTL")),
            "fresh"
        );
    }

    /// Hammer the cache from many threads while a refresh is slow:
    /// concurrent reads must never deadlock and never run more than one
    /// refresh at a time.  The in-flight assertion lives inside the shared
    /// source so it holds regardless of which caller claims the slot and
    /// how the threads interleave.
    #[test]
    fn concurrent_stale_reads_do_not_deadlock() {
        const N_READERS: usize = 8;
        const N_READS: usize = 500;

        let queries = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel::<()>();

        // Every caller passes the same source to `get_with`; the vast
        // majority of calls never run it (fresh or stale-served), but any
        // caller that does claim the refresh slot gets this slow source.
        let src_queries = Arc::clone(&queries);
        let src_in_flight = Arc::clone(&in_flight);
        let make_source = Arc::new(move || {
            let queries = Arc::clone(&src_queries);
            let in_flight = Arc::clone(&src_in_flight);
            let started_tx = started_tx.clone();
            move || {
                queries.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    in_flight.fetch_add(1, Ordering::SeqCst),
                    0,
                    "two refreshes ran concurrently",
                );
                // Unbounded channel: `send` never blocks.
                let _ = started_tx.send(());
                // Simulate a slow round-trip.
                std::thread::sleep(Duration::from_millis(20));
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Arc::from("slow")
            }
        });

        let value =
            Arc::new(TtlValue::<Arc<str>>::new(TTL, Arc::from("seed")));
        value.invalidate();

        let readers: Vec<_> = (0..N_READERS)
            .map(|_| {
                let value = Arc::clone(&value);
                let make_source = Arc::clone(&make_source);
                std::thread::spawn(move || {
                    for _ in 0..N_READS {
                        let served = value.get_with(make_source());
                        assert!(!served.is_empty());
                    }
                })
            })
            .collect();

        // At least one refresh was in flight while the readers hammered;
        // the readers finishing without deadlock is verified by the joins.
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("no refresh ever started");
        for reader in readers {
            reader.join().unwrap();
        }
        assert!(queries.load(Ordering::SeqCst) >= 1);
    }

    /// If the source panics, the refresh slot must still be released so a
    /// later caller can refresh; the cache cannot be wedged permanently.
    #[test]
    fn panicking_refresh_does_not_wedge_the_cache() {
        let panicked = Arc::new(AtomicBool::new(false));
        let src_panicked = Arc::clone(&panicked);
        let value = TtlValue::<Arc<str>>::new(TTL, Arc::from("seed"));
        value.invalidate();

        // Silence the expected panic message; restore the previous hook
        // afterwards.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                value.get_with(move || {
                    src_panicked.store(true, Ordering::SeqCst);
                    panic!("source failure");
                });
            }));
        std::panic::set_hook(prev_hook);
        assert!(outcome.is_err());
        assert!(panicked.load(Ordering::SeqCst));

        // The slot guard's `Drop` released the refresh slot, so the next
        // caller can refresh.
        assert_eq!(&*value.get_with(|| Arc::from("recovered")), "recovered");
    }

    #[test]
    fn invalidate_forces_refresh_even_for_fresh_entries() {
        let value = TtlValue::<Arc<str>>::new(TTL, Arc::from("seed"));
        assert_eq!(&*value.get_with(|| unreachable!("fresh entry")), "seed");
        value.invalidate();
        assert_eq!(&*value.get_with(|| Arc::from("refreshed")), "refreshed");
    }
}
