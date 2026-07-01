// Created: 2026-06-10 by Constructor Tech
//! Polling prefix-watch polyfill (DESIGN §3.12).
//!
//! Synthesizes [`watch_prefix`](crate::cache::ClusterCacheBackend::watch_prefix)
//! semantics on backends that declare `features().prefix_watch == false`, for
//! consumers that need prefix notifications regardless of the bound backend.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::cache::backend::ClusterCacheBackend;
use crate::cache::types::CacheEvent;
use crate::cache::watch::{CacheWatch, CacheWatchEvent, CacheWatchSender};
use crate::error::{ClusterError, ProviderErrorKind};

/// In-flight buffer for the synthesized watch. Generous so a burst of diffs in
/// one interval is not dropped as `Lagged`.
const POLL_BUFFER: usize = 256;

/// An opt-in polling prefix watch that approximates a native prefix watch.
///
/// [`spawn`](Self::spawn) starts a background task that, on each interval tick,
/// lists the keys under `prefix` via
/// [`scan_prefix`](crate::cache::ClusterCacheBackend::scan_prefix), reads each
/// key's version with a `get`, and diffs against the previous listing — emitting
/// [`CacheEvent::Changed`] for new or version-bumped keys and
/// [`CacheEvent::Deleted`] for keys that disappeared.
///
/// Emitted events carry whatever key `scan_prefix` returns: against a bare
/// backend that is the full backend key; against a scoped cache
/// ([`ClusterCacheV1::scoped`](crate::cache::ClusterCacheV1::scoped) →
/// [`watch_prefix_polling`](crate::cache::ClusterCacheV1::watch_prefix_polling))
/// the scope wrapper has already stripped the prefix, so it is the
/// consumer-relative key — the same as a native scoped watch.
///
/// A scan-diff cannot distinguish a TTL expiry from an explicit delete, so a key
/// that disappears is always reported as [`CacheEvent::Deleted`], never
/// [`CacheEvent::Expired`] — a behavioral divergence from a native prefix watch.
///
/// # Cost
///
/// This is **not free**: each interval performs one `scan_prefix` plus one `get`
/// per key (`N + 1` round-trips, with gets issued concurrently), and detection
/// latency is bounded by `interval` — there is no millisecond-level precision
/// and rapid create-then-delete churn within one interval can be missed. Prefer a
/// backend with **native** prefix-watch support (`features().prefix_watch == true`)
/// at scale; reach for this polyfill only when the bound backend lacks it.
///
/// # Lifecycle
///
/// Dropping the returned [`CacheWatch`] stops the polling task on its next tick
/// (it checks `is_closed()` each tick, so even a quiescent keyspace with nothing
/// to emit still terminates). A backend error is surfaced as
/// [`CacheWatchEvent::Closed`] and ends the task — the polyfill does **not**
/// retry internally, so wrap it with the watch auto-restart combinator
/// (`CacheWatch::auto_restart`, DECOMPOSITION §2.8) to recover transparently from
/// a retryable [`ProviderErrorKind`](crate::error::ProviderErrorKind).
///
/// # Capability
///
/// Intended for backends declaring `features().prefix_watch == false`; the caller
/// is responsible for choosing the polyfill only when native `watch_prefix` is
/// unavailable (this constructor does not inspect `features()`).
pub struct PollingPrefixWatch;

impl PollingPrefixWatch {
    /// Starts the polling task and returns the consumer-facing [`CacheWatch`].
    ///
    /// A zero `interval` is rejected at the watch itself: rather than panic
    /// `tokio::time::interval` inside the spawned task (which would drop the
    /// sender and leave the consumer's `recv()` returning `None` with no error
    /// — a silently dead watch), the returned watch yields a single terminal
    /// [`CacheWatchEvent::Closed`] carrying
    /// [`ClusterError::InvalidConfig`](crate::error::ClusterError::InvalidConfig).
    /// That close is non-retryable, so wrapping with `CacheWatch::auto_restart`
    /// propagates it rather than reconnecting.
    #[must_use]
    pub fn spawn(
        cache: Arc<dyn ClusterCacheBackend>,
        prefix: &str,
        interval: Duration,
    ) -> CacheWatch {
        let (sender, watch) = CacheWatch::channel(POLL_BUFFER);
        if interval.is_zero() {
            tokio::spawn(async move {
                sender
                    .send(CacheWatchEvent::Closed(
                        crate::error::ClusterError::InvalidConfig {
                            reason: "watch_prefix_polling requires a non-zero poll interval"
                                .to_owned(),
                        },
                    ))
                    .await
                    .ok();
            });
        } else {
            let prefix = prefix.to_owned();
            tokio::spawn(poll_loop(cache, prefix, interval, sender));
        }
        watch
    }
}

/// The background diff loop. Exits when the consumer drops the watch (a `send`
/// fails) or a backend call errors (after emitting a terminal `Closed`).
async fn poll_loop(
    cache: Arc<dyn ClusterCacheBackend>,
    prefix: String,
    interval: Duration,
    sender: CacheWatchSender,
) {
    let mut ticker = tokio::time::interval(interval);
    // If a slow snapshot (large keyspace × N+1 round-trips) overruns the
    // interval, pace the next tick rather than firing a catch-up burst.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // key -> last observed version.
    let mut previous: HashMap<String, u64> = HashMap::new();
    loop {
        ticker.tick().await;
        // Stop if the consumer dropped the watch. Checked every tick — not only
        // on a failed `send` — so a quiescent keyspace (no diffs to send) still
        // terminates the task rather than polling forever.
        if sender.is_closed() {
            return;
        }
        let current = match snapshot(&cache, &prefix).await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                // Surface a terminal error and stop, like a closed native watch.
                sender.send(CacheWatchEvent::Closed(err)).await.ok();
                return;
            }
        };
        // New or version-bumped keys → Changed.
        for (key, version) in &current {
            if previous.get(key) != Some(version)
                && sender
                    .send(CacheWatchEvent::Event(CacheEvent::Changed {
                        key: key.clone(),
                    }))
                    .await
                    .is_err()
            {
                return;
            }
        }
        // Keys that disappeared since the previous listing → Deleted.
        for key in previous.keys() {
            if !current.contains_key(key)
                && sender
                    .send(CacheWatchEvent::Event(CacheEvent::Deleted {
                        key: key.clone(),
                    }))
                    .await
                    .is_err()
            {
                return;
            }
        }
        previous = current;
    }
}

/// Maximum number of concurrent `get` calls issued by [`snapshot`]. Bounds
/// task fan-out so a large keyspace does not exhaust the thread pool.
const SNAPSHOT_CONCURRENCY: usize = 32;

/// Lists the keys under `prefix` and reads each key's current version
/// concurrently, bounded to [`SNAPSHOT_CONCURRENCY`] in-flight gets at a time.
/// A key that vanishes between the scan and its `get` is simply omitted from
/// the snapshot.
async fn snapshot(
    cache: &Arc<dyn ClusterCacheBackend>,
    prefix: &str,
) -> Result<HashMap<String, u64>, ClusterError> {
    let keys = cache.scan_prefix(prefix).await?;
    let sem = Arc::new(tokio::sync::Semaphore::new(SNAPSHOT_CONCURRENCY));
    let handles: Vec<_> = keys
        .into_iter()
        .map(|key| {
            let cache = Arc::clone(cache);
            let sem = Arc::clone(&sem);
            tokio::spawn(async move {
                let _permit = sem.acquire_owned().await;
                let result = cache.get(&key).await;
                result.map(|opt| (key, opt.map(|e| e.version)))
            })
        })
        .collect();
    let mut snapshot = HashMap::with_capacity(handles.len());
    for handle in handles {
        match handle.await {
            Ok(Ok((key, Some(version)))) => {
                snapshot.insert(key, version);
            }
            Ok(Ok((_, None))) => {}
            Ok(Err(err)) => return Err(err),
            Err(join_err) => {
                return Err(ClusterError::Provider {
                    kind: ProviderErrorKind::Other,
                    message: format!("concurrent snapshot task panicked: {join_err}"),
                });
            }
        }
    }
    Ok(snapshot)
}

#[cfg(test)]
#[path = "polyfill_tests.rs"]
mod polyfill_tests;
