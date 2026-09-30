// ---------------------------------------------------------------------------
// Copyright:   (c) 2026 ff. Michael Amrhein (michael@adrhinum.de)
// License:     This program is part of a larger application. For license
//              details please read the file LICENSE.TXT provided together
//              with the application.
// ---------------------------------------------------------------------------
// $Source$
// $Revision$

//! Focused-application tracking for the daemon runtime.
//!
//! [`FocusTracker`] answers "which application is in the foreground?" for
//! every key event without paying the cost of the platform query on the
//! keystroke hot path.  It composes the generic [`TtlValue`] cache (short
//! TTL, single-flight refresh) with the injectable platform source, keeping
//! the active-app responsibility out of
//! [`RuntimeState`](crate::daemon::state::RuntimeState).

use std::{fmt, sync::Arc, time::Duration};

use crate::common::ttl_value::TtlValue;

/// How long a cached active-app name stays fresh.
///
/// The platform query is expensive (a synchronous D-Bus round-trip on
/// Wayland, potentially establishing a new connection), while keyboard
/// focus changes are rare.  A short TTL keeps per-event lookups cheap
/// without lagging focus switches in any perceptible way.
const ACTIVE_APP_TTL: Duration = Duration::from_millis(100);

/// The currently focused application name, served from a short-TTL cache.
///
/// The refresh — a blocking X11 round-trip, D-Bus call, or compositor
/// socket read — is single-flight and runs outside the cache lock, so a
/// stalled IPC peer can at worst cause brief staleness, never a lock-up of
/// the keystroke hot path.  The cache mechanics live in [`TtlValue`].
pub(crate) struct FocusTracker {
    value: TtlValue<Arc<str>>,
    /// Injectable source for the active application name.  The daemon
    /// binary wires this to the platform query; tests can supply a fixed
    /// value.  Kept as a closure so the daemon never references platform
    /// code directly.
    source: Box<dyn Fn() -> String + Send + Sync>,
}

impl FocusTracker {
    pub(super) fn new(source: Box<dyn Fn() -> String + Send + Sync>) -> Self {
        Self {
            // Seed with "unknown": the first `ACTIVE_APP_TTL` of daemon
            // startup serves "unknown" without a platform query, which
            // only skips app-scoped rules for that brief window.
            value: TtlValue::new(ACTIVE_APP_TTL, Arc::from("unknown")),
            source,
        }
    }

    /// Name of the currently foreground application.
    ///
    /// If the cached entry has expired, exactly one caller refreshes it
    /// through the platform source while any concurrent caller receives the
    /// last known value; see [`TtlValue::get_with`].
    pub(super) fn get(&self) -> Arc<str> {
        self.value.get_with(|| Arc::from((self.source)()))
    }

    /// Force the cached entry stale without waiting for the TTL to elapse.
    #[cfg(test)]
    pub(super) fn expire(&self) {
        self.value.invalidate();
    }
}

impl fmt::Debug for FocusTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FocusTracker")
            .field("value", &self.value)
            .field("source", &"<fn>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[test]
    fn seeded_name_is_served_without_querying() {
        let calls = Arc::new(AtomicUsize::new(0));
        let src_calls = Arc::clone(&calls);
        let tracker = FocusTracker::new(Box::new(move || {
            src_calls.fetch_add(1, Ordering::SeqCst);
            "queried_app".to_string()
        }));

        // The entry seeded by `new` is fresh, so no query runs.
        assert_eq!(&*tracker.get(), "unknown");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn expired_entry_queries_source_once_and_is_cached() {
        let calls = Arc::new(AtomicUsize::new(0));
        let src_calls = Arc::clone(&calls);
        let tracker = FocusTracker::new(Box::new(move || {
            let n = src_calls.fetch_add(1, Ordering::SeqCst);
            format!("app{n}")
        }));
        tracker.expire();

        assert_eq!(&*tracker.get(), "app0");
        // Serving the refreshed entry within the TTL must not re-query.
        assert_eq!(&*tracker.get(), "app0");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
