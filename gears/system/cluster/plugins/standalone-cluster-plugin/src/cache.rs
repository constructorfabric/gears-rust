// Created: 2026-06-11 by Constructor Tech
//! The native in-process [`ClusterCacheBackend`] backing the standalone plugin.
//!
//! A `HashMap`-backed store with monotonic per-key versioning, lazy TTL (an
//! expired entry reads as absent immediately), a background sweeper that emits
//! [`CacheEvent::Expired`] to matching watchers, and exact + prefix watches with
//! closed-channel pruning. It declares [`CacheConsistency::Linearizable`] and
//! native prefix-watch, which is what the SDK default leader-election and lock
//! backends require (their consistency guard rejects an eventually-consistent
//! cache).
//!
//! Productionized from the SDK's `defaults::test_cache::MemoryCache` fixture: the
//! store and watch logic are the same, but the sweeper is driven by an explicit
//! [`CancellationToken`] so the plugin handle can stop it deterministically.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, CacheWatch, CacheWatchEvent,
    CacheWatchSender, CacheWatchTrySendError, ClusterCacheBackend, ClusterError,
};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Per-watch in-flight buffer. Generous so a renewal/heartbeat storm is
/// absorbed without lag under normal in-process load. On overflow the writer is
/// never blocked: `broadcast` drops the event for the lagging watcher and
/// coalesces the drops into a `CacheWatchEvent::Lagged` delivered once the
/// buffer drains.
const WATCH_CAPACITY: usize = 256;

/// A stored value with its version and optional expiry deadline.
struct Stored {
    value: Vec<u8>,
    version: u64,
    expires_at: Option<Instant>,
}

impl Stored {
    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|deadline| deadline <= now)
    }

    fn entry(&self) -> CacheEntry {
        CacheEntry {
            value: self.value.clone(),
            version: self.version,
        }
    }
}

/// The subscription kind a watcher matches keys against.
enum WatchKind {
    Exact(String),
    Prefix(String),
}

impl WatchKind {
    fn matches(&self, key: &str) -> bool {
        match self {
            Self::Exact(exact) => exact == key,
            Self::Prefix(prefix) => key.starts_with(prefix.as_str()),
        }
    }
}

/// One live watch subscription, identified so a failed send can prune it.
struct Watcher {
    id: u64,
    kind: WatchKind,
    sender: CacheWatchSender,
    /// Events dropped because this watcher's buffer was full, awaiting a
    /// `Lagged` notice once space frees. Zero in the common (keeping-up) case.
    dropped: u64,
}

/// The cache's locked interior.
struct Inner {
    map: HashMap<String, Stored>,
    watchers: Vec<Watcher>,
    next_watch_id: u64,
    /// Latched by [`StandaloneCache::shutdown`] on graceful shutdown. Once set,
    /// mutating ops and new watches are rejected with [`ClusterError::Shutdown`]
    /// so a racing op cannot resurrect a watcher after the close broadcast.
    shutting_down: bool,
}

/// The native in-process cache backend.
///
/// Create with [`new`](Self::new) and start its TTL sweeper with
/// [`spawn_sweeper`](Self::spawn_sweeper); the
/// [`StandaloneClusterHandle`](crate::StandaloneClusterHandle) wires both
/// together and owns their lifecycle.
pub struct StandaloneCache {
    inner: Mutex<Inner>,
}

impl StandaloneCache {
    /// Creates an empty cache. The TTL sweeper is started separately via
    /// [`spawn_sweeper`](Self::spawn_sweeper).
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                watchers: Vec::new(),
                next_watch_id: 0,
                shutting_down: false,
            }),
        })
    }

    /// Spawns the background TTL sweeper, returning its task handle. The sweeper
    /// holds only a [`Weak`] reference, so it also self-terminates if the cache is
    /// dropped; `shutdown` lets the owning handle stop it deterministically.
    pub fn spawn_sweeper(
        self: &Arc<Self>,
        interval: Duration,
        shutdown: CancellationToken,
    ) -> JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(sweep_loop(weak, interval, shutdown))
    }

    /// Locks the interior, recovering from a poisoned lock rather than panicking.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Closes every active watch terminally on graceful shutdown
    /// (`cpt-cf-clst-fr-shutdown-revoke`, DESIGN §3.13). Latches the
    /// shutting-down flag, then best-effort delivers a
    /// [`CacheWatchEvent::Closed(ClusterError::Shutdown)`] to each watcher and
    /// drops them.
    ///
    /// Non-blocking: a watcher whose consumer has dropped the watch (a closed
    /// channel) is simply pruned, and a full buffer is ignored — the consumer
    /// will still observe end-of-stream when the sender drops here. After this
    /// returns, mutating ops and new watches are rejected (see the per-op
    /// guards), so a racing op cannot register a watcher that would miss the
    /// close. Reads (`get`/`contains`/`scan_prefix`) stay live — they cannot
    /// resurrect a watcher and a harmless read during teardown is preferable to
    /// a spurious error.
    pub fn shutdown(&self) {
        let mut guard = self.lock();
        guard.shutting_down = true;
        // Drain the watchers under the lock: once removed, no future broadcast
        // can reference them, and dropping their senders at the end of this scope
        // ends each stream even if the `Closed` event could not be delivered.
        let watchers = std::mem::take(&mut guard.watchers);
        for watcher in &watchers {
            // Best-effort: a dropped consumer (Closed) or a full buffer (Full)
            // is ignored — the sender drop below still ends the stream.
            let _sent = watcher
                .sender
                .try_send(CacheWatchEvent::Closed(ClusterError::Shutdown));
        }
    }

    /// Sends `event` to every watcher matching `key` without ever blocking the
    /// caller, pruning any whose consumer has dropped the watch.
    ///
    /// Uses the non-blocking [`CacheWatchSender::try_send`]: a watcher whose
    /// buffer is full has the event dropped and its lag counter incremented; its
    /// next successful delivery is preceded by a [`CacheWatchEvent::Lagged`] so
    /// the consumer knows to re-read. Because `try_send` never awaits, the lock
    /// is held for the whole pass and a slow consumer can never stall the write
    /// path of other writers on this cache (DESIGN §3.9 at-most-once delivery).
    fn broadcast(&self, key: &str, event: &CacheEvent) {
        let mut guard = self.lock();
        let mut dead = Vec::new();
        for watcher in guard.watchers.iter_mut().filter(|w| w.kind.matches(key)) {
            // Flush a pending lag notice first so the consumer re-reads before it
            // sees the next event.
            if watcher.dropped > 0 {
                match watcher.sender.try_send(CacheWatchEvent::Lagged {
                    dropped: watcher.dropped,
                }) {
                    Ok(()) => watcher.dropped = 0,
                    Err(CacheWatchTrySendError::Full) => {
                        watcher.dropped += 1;
                        continue;
                    }
                    Err(CacheWatchTrySendError::Closed) => {
                        dead.push(watcher.id);
                        continue;
                    }
                }
            }
            match watcher
                .sender
                .try_send(CacheWatchEvent::Event(event.clone()))
            {
                Ok(()) => {}
                Err(CacheWatchTrySendError::Full) => watcher.dropped += 1,
                Err(CacheWatchTrySendError::Closed) => dead.push(watcher.id),
            }
        }
        if !dead.is_empty() {
            guard.watchers.retain(|watcher| !dead.contains(&watcher.id));
        }
    }

    /// Removes every expired entry and emits an `Expired` event for each.
    fn sweep_expired(&self) {
        let now = Instant::now();
        let expired: Vec<String> = {
            let mut guard = self.lock();
            let keys: Vec<String> = guard
                .map
                .iter()
                .filter(|(_, stored)| stored.is_expired(now))
                .map(|(key, _)| key.clone())
                .collect();
            for key in &keys {
                guard.map.remove(key);
            }
            keys
        };
        for key in expired {
            self.broadcast(&key, &CacheEvent::Expired { key: key.clone() });
        }
    }

    /// Registers a new watch, or rejects it with [`ClusterError::Shutdown`] once
    /// the cache is shutting down so a racing subscribe cannot register a watcher
    /// the [`shutdown`](Self::shutdown) close pass has already gone past.
    fn register_watch(&self, kind: WatchKind) -> Result<CacheWatch, ClusterError> {
        let (sender, watch) = CacheWatch::channel(WATCH_CAPACITY);
        let mut guard = self.lock();
        if guard.shutting_down {
            return Err(ClusterError::Shutdown);
        }
        let id = guard.next_watch_id;
        guard.next_watch_id += 1;
        guard.watchers.push(Watcher {
            id,
            kind,
            sender,
            dropped: 0,
        });
        Ok(watch)
    }
}

/// The detached sweeper driving TTL expiry; exits on `shutdown` or once the cache
/// is dropped (the `Weak` no longer upgrades).
async fn sweep_loop(weak: Weak<StandaloneCache>, interval: Duration, shutdown: CancellationToken) {
    let mut ticker = tokio::time::interval(interval);
    // A slow sweep should not fire a catch-up burst on the next tick.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                let Some(cache) = weak.upgrade() else {
                    return;
                };
                cache.sweep_expired();
            }
        }
    }
}

#[async_trait]
impl ClusterCacheBackend for StandaloneCache {
    fn consistency(&self) -> CacheConsistency {
        // An in-process store is linearizable: every operation observes the
        // effect of all prior operations.
        CacheConsistency::Linearizable
    }

    fn features(&self) -> CacheFeatures {
        // Native prefix watch (and `scan_prefix`) are supported.
        CacheFeatures::new(true)
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        let now = Instant::now();
        let guard = self.lock();
        Ok(match guard.map.get(key) {
            Some(stored) if !stored.is_expired(now) => Some(stored.entry()),
            _ => None,
        })
    }

    async fn put(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<(), ClusterError> {
        let now = Instant::now();
        {
            let mut guard = self.lock();
            if guard.shutting_down {
                return Err(ClusterError::Shutdown);
            }
            let version = match guard.map.get(key) {
                Some(stored) if !stored.is_expired(now) => stored.version + 1,
                _ => 1,
            };
            guard.map.insert(
                key.to_owned(),
                Stored {
                    value: value.to_vec(),
                    version,
                    expires_at: ttl.map(|d| now + d),
                },
            );
        }
        self.broadcast(
            key,
            &CacheEvent::Changed {
                key: key.to_owned(),
            },
        );
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        let now = Instant::now();
        let was_live = {
            let mut guard = self.lock();
            if guard.shutting_down {
                return Err(ClusterError::Shutdown);
            }
            let live = matches!(guard.map.get(key), Some(stored) if !stored.is_expired(now));
            guard.map.remove(key);
            live
        };
        if was_live {
            self.broadcast(
                key,
                &CacheEvent::Deleted {
                    key: key.to_owned(),
                },
            );
        }
        Ok(was_live)
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        let now = Instant::now();
        let guard = self.lock();
        Ok(matches!(guard.map.get(key), Some(stored) if !stored.is_expired(now)))
    }

    async fn put_if_absent(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        let now = Instant::now();
        let created = {
            let mut guard = self.lock();
            if guard.shutting_down {
                return Err(ClusterError::Shutdown);
            }
            if matches!(guard.map.get(key), Some(stored) if !stored.is_expired(now)) {
                None
            } else {
                let stored = Stored {
                    value: value.to_vec(),
                    version: 1,
                    expires_at: ttl.map(|d| now + d),
                };
                let entry = stored.entry();
                guard.map.insert(key.to_owned(), stored);
                Some(entry)
            }
        };
        if created.is_some() {
            self.broadcast(
                key,
                &CacheEvent::Changed {
                    key: key.to_owned(),
                },
            );
        }
        Ok(created)
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected_version: u64,
        new_value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<CacheEntry, ClusterError> {
        let now = Instant::now();
        let outcome = {
            let mut guard = self.lock();
            if guard.shutting_down {
                return Err(ClusterError::Shutdown);
            }
            match guard.map.get(key) {
                Some(stored) if !stored.is_expired(now) => {
                    if stored.version == expected_version {
                        let version = stored.version + 1;
                        let stored = Stored {
                            value: new_value.to_vec(),
                            version,
                            expires_at: ttl.map(|d| now + d),
                        };
                        let entry = stored.entry();
                        guard.map.insert(key.to_owned(), stored);
                        Ok(entry)
                    } else {
                        Err(ClusterError::CasConflict {
                            key: key.to_owned(),
                            current: Some(stored.entry()),
                        })
                    }
                }
                _ => Err(ClusterError::CasConflict {
                    key: key.to_owned(),
                    current: None,
                }),
            }
        };
        if outcome.is_ok() {
            self.broadcast(
                key,
                &CacheEvent::Changed {
                    key: key.to_owned(),
                },
            );
        }
        outcome
    }

    async fn compare_and_delete(
        &self,
        key: &str,
        expected_value: &[u8],
    ) -> Result<bool, ClusterError> {
        let now = Instant::now();
        let deleted = {
            let mut guard = self.lock();
            if guard.shutting_down {
                return Err(ClusterError::Shutdown);
            }
            match guard.map.get(key) {
                Some(stored)
                    if !stored.is_expired(now) && stored.value.as_slice() == expected_value =>
                {
                    guard.map.remove(key);
                    true
                }
                // A value mismatch or an absent/expired key is a safe no-op: a
                // successor that re-claimed after our TTL lapsed wrote a different
                // value, so its fresh claim is never wiped.
                _ => false,
            }
        };
        if deleted {
            self.broadcast(
                key,
                &CacheEvent::Deleted {
                    key: key.to_owned(),
                },
            );
        }
        Ok(deleted)
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        self.register_watch(WatchKind::Exact(key.to_owned()))
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        self.register_watch(WatchKind::Prefix(prefix.to_owned()))
    }

    async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        let now = Instant::now();
        let guard = self.lock();
        Ok(guard
            .map
            .iter()
            .filter(|(key, stored)| key.starts_with(prefix) && !stored.is_expired(now))
            .map(|(key, _)| key.clone())
            .collect())
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod cache_tests;
