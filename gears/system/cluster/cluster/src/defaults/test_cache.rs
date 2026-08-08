//! A functional in-memory [`ClusterCacheBackend`] fixture for unit-testing the
//! SDK default backends (`#[cfg(test)]` only).
//!
//! Unlike the contract-level stub backends in the primitive modules, the default
//! backends contain real logic — renewal, conditional release, heartbeat
//! convergence — so testing them needs a cache that genuinely implements
//! `put_if_absent` / `compare_and_swap` / `watch` / `watch_prefix` with
//! per-key versioning and TTL expiry. This fixture provides exactly that:
//!
//! - a `HashMap`-backed store with a monotonic per-key version (reset to `1` on
//!   a fresh create, incremented on each overwrite/CAS);
//! - lazy TTL handling — an expired entry reads as absent immediately, so a
//!   direct accessor (e.g. `put_if_absent` reaping a crashed lock holder) never
//!   observes a stale value;
//! - a background sweeper that, on a fine interval, removes expired entries and
//!   emits [`CacheEvent::Expired`] to matching watchers so a watch-driven waiter
//!   wakes on TTL expiry;
//! - exact and prefix watches with closed-channel pruning.
//!
//! Tests drive it with [`tokio::time::pause`] + `advance` to exercise renewal
//! and TTL deterministically without real sleeps.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;

use cluster_sdk::cache::types::{PutRequest, Ttl};
use cluster_sdk::cache::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, CacheWatch, CacheWatchEvent,
    CacheWatchSender, ClusterCacheBackend,
};
use cluster_sdk::error::ClusterError;

/// How often the background sweeper scans for expired entries. Fine enough that
/// a `tokio::time::advance` past a TTL deterministically triggers an `Expired`
/// emission, coarse enough to avoid needless wakeups.
const SWEEP_INTERVAL: Duration = Duration::from_millis(25);

/// Per-watch in-flight buffer. Generous so a renewal/heartbeat storm in a test
/// never drops events as `Lagged`.
const WATCH_CAPACITY: usize = 256;

/// How long a [`WatchBehavior::Rotating`] exact watch lives before ending its
/// stream. Comfortably above the blocking-lock loop's re-subscribe backoff so it
/// is classified as a legitimate end-of-stream, not a busy-spin.
const ROTATING_WATCH_LIFETIME: Duration = Duration::from_millis(200);

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
}

/// The fixture's locked interior.
struct Inner {
    map: HashMap<String, Stored>,
    watchers: Vec<Watcher>,
    next_watch_id: u64,
    /// Senders for prefix watches handed out by
    /// [`MemoryCache::linearizable_with_silent_prefix_watch`], parked here
    /// unregistered so nothing ever matches them.
    ///
    /// Held rather than dropped on purpose: a dropped sender ends the consumer's
    /// stream, which makes a service-discovery maintainer exit and a later call
    /// start a fresh one that reconciles from backend truth — the very path such a
    /// fixture exists to keep closed. Keeping them alive means `recv` simply never
    /// resolves, so the maintained view can only ever be populated by an explicit
    /// reconcile.
    parked_senders: Vec<CacheWatchSender>,
}

/// How a fixture's exact [`watch`](ClusterCacheBackend::watch) behaves, for the
/// blocking-lock re-subscribe tests.
#[derive(Clone, Copy)]
enum WatchBehavior {
    /// A live watch registered against the store.
    Normal,
    /// The sender is dropped at once, so `recv` yields `None` immediately — an
    /// unusable watch that would busy-spin the blocking loop.
    DeadImmediate,
    /// The watch delivers no events and ends (`recv` → `None`) only after a
    /// meaningful interval — a legitimate stream rotation.
    Rotating,
}

/// How a fixture's [`watch_prefix`](ClusterCacheBackend::watch_prefix) behaves.
///
/// One enum rather than two booleans, because the three states are alternatives
/// rather than independent switches — and because the declaration
/// (`features().prefix_watch`) and the delivery behaviour have to agree, which two
/// bools leave free to disagree.
#[derive(Clone, Copy)]
enum PrefixWatchBehavior {
    /// A live prefix watch registered against the store, delivering on every
    /// matching write. `features().prefix_watch` is `true`.
    Native,
    /// `features().prefix_watch` is `true` and `watch_prefix` succeeds, but the
    /// watch **never delivers** — the stand-in for a *remote* prefix watch whose
    /// events are still in flight.
    ///
    /// The distinction this exists to make testable: `features().prefix_watch`
    /// says whether a backend *has* a native prefix watch, not whether its
    /// delivery is synchronous. Every in-process cache conflates the two, because
    /// delivery is a task yield; a Redis-backed one does not, because delivery is
    /// a network round trip. See
    /// [`linearizable_with_silent_prefix_watch`](MemoryCache::linearizable_with_silent_prefix_watch).
    Silent,
    /// `features().prefix_watch` is `false` and `watch_prefix` answers
    /// [`ClusterError::Unsupported`] — the service-discovery degradation path.
    Unsupported,
}

/// A functional in-memory cache backend for default-backend unit tests.
pub struct MemoryCache {
    inner: Mutex<Inner>,
    consistency: CacheConsistency,
    prefix_watch: PrefixWatchBehavior,
    watch_behavior: WatchBehavior,
    /// When prefix watch is [`PrefixWatchBehavior::Unsupported`], suspend once
    /// (`yield_now`) before returning `Unsupported` from `watch_prefix`. Lets a
    /// test deterministically interleave two concurrent `register`s at the
    /// maintainer-startup await.
    slow_unsupported_watch: bool,
}

impl MemoryCache {
    /// A linearizable cache with native prefix-watch support — the common case.
    /// `pub(crate)` for the same reason as
    /// [`eventually_consistent`](Self::eventually_consistent): the wiring tests
    /// build a *native* backend over a linearizable cache to stand beside a weak
    /// one, proving a native binding needs no opt-in.
    pub(crate) fn linearizable() -> Arc<Self> {
        Self::spawn(CacheConsistency::Linearizable, PrefixWatchBehavior::Native)
    }

    /// An eventually-consistent cache, for exercising the constructor guard.
    /// `pub(crate)` rather than `pub(super)` because the wiring's own tests need it
    /// too: it is the stand-in for the Redis cache in the weak-consistency opt-in
    /// tests (`config_tests.rs`, `wiring_tests.rs`), Redis being the first backend
    /// in the workspace to declare `EventuallyConsistent`.
    pub(crate) fn eventually_consistent() -> Arc<Self> {
        Self::spawn(
            CacheConsistency::EventuallyConsistent,
            PrefixWatchBehavior::Native,
        )
    }

    /// A linearizable cache that declares no native prefix watch, so
    /// `watch_prefix` returns [`ClusterError::Unsupported`] — for the
    /// service-discovery degradation path.
    pub fn linearizable_without_prefix_watch() -> Arc<Self> {
        Self::spawn(
            CacheConsistency::Linearizable,
            PrefixWatchBehavior::Unsupported,
        )
    }

    /// A linearizable cache whose exact `watch` ends immediately on every
    /// subscribe — exercises the blocking-lock re-subscribe cap against a
    /// backend with an unusable watch.
    pub(super) fn linearizable_with_dead_watch() -> Arc<Self> {
        Self::spawn_with(
            CacheConsistency::Linearizable,
            PrefixWatchBehavior::Native,
            WatchBehavior::DeadImmediate,
        )
    }

    /// A linearizable cache whose exact `watch` ends only after a meaningful
    /// interval (a legitimate stream rotation) — verifies the blocking-lock loop
    /// keeps waiting across rotations instead of tripping the unusable-watch cap.
    pub(super) fn linearizable_with_rotating_watch() -> Arc<Self> {
        Self::spawn_with(
            CacheConsistency::Linearizable,
            PrefixWatchBehavior::Native,
            WatchBehavior::Rotating,
        )
    }

    /// A linearizable cache that **declares** native prefix watch but whose
    /// prefix watches never deliver an event — the stand-in for a remote backend
    /// whose `Changed` notification has not arrived yet.
    ///
    /// The one property it isolates, deterministically and with no sleeps: a
    /// service-discovery view maintained purely from prefix-watch events is not a
    /// sound source for `discover`, because on a remote backend those events lag
    /// the writes that produced them. A `discover` that trusted the view here
    /// would never see an instance registered by another process at all; one that
    /// reconciles from `scan_prefix` sees it immediately.
    ///
    /// Deliberately not modelled as a *slow* watch. A delay would make the test a
    /// race that usually fails rather than a statement that always holds, and the
    /// property under test is about the source of truth, not about latency.
    pub fn linearizable_with_silent_prefix_watch() -> Arc<Self> {
        Self::spawn_full(
            CacheConsistency::Linearizable,
            PrefixWatchBehavior::Silent,
            WatchBehavior::Normal,
            false,
        )
    }

    fn spawn(consistency: CacheConsistency, prefix_watch: PrefixWatchBehavior) -> Arc<Self> {
        Self::spawn_with(consistency, prefix_watch, WatchBehavior::Normal)
    }

    fn spawn_with(
        consistency: CacheConsistency,
        prefix_watch: PrefixWatchBehavior,
        watch_behavior: WatchBehavior,
    ) -> Arc<Self> {
        Self::spawn_full(consistency, prefix_watch, watch_behavior, false)
    }

    fn spawn_full(
        consistency: CacheConsistency,
        prefix_watch: PrefixWatchBehavior,
        watch_behavior: WatchBehavior,
        slow_unsupported_watch: bool,
    ) -> Arc<Self> {
        let cache = Arc::new(Self {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                watchers: Vec::new(),
                next_watch_id: 0,
                parked_senders: Vec::new(),
            }),
            consistency,
            prefix_watch,
            watch_behavior,
            slow_unsupported_watch,
        });
        // The sweeper holds only a weak reference, so it self-terminates once
        // the test drops the cache.
        let weak = Arc::downgrade(&cache);
        tokio::spawn(sweep_loop(weak));
        cache
    }

    /// Locks the interior, recovering from a poisoned lock rather than panicking
    /// (the fixture must not `unwrap`/`expect`).
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Sends `event` to every watcher matching `key`, pruning any whose consumer
    /// has dropped the watch.
    async fn broadcast(&self, key: &str, event: CacheEvent) {
        let targets: Vec<(u64, CacheWatchSender)> = {
            let guard = self.lock();
            guard
                .watchers
                .iter()
                .filter(|watcher| watcher.kind.matches(key))
                .map(|watcher| (watcher.id, watcher.sender.clone()))
                .collect()
        };
        let mut dead = Vec::new();
        for (id, sender) in targets {
            if sender
                .send(CacheWatchEvent::Event(event.clone()))
                .await
                .is_err()
            {
                dead.push(id);
            }
        }
        if !dead.is_empty() {
            self.lock()
                .watchers
                .retain(|watcher| !dead.contains(&watcher.id));
        }
    }

    /// Removes every expired entry and emits an `Expired` event for each.
    async fn sweep_expired(&self) {
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
            self.broadcast(&key, CacheEvent::Expired { key: key.clone() })
                .await;
        }
    }

    fn register_watch(&self, kind: WatchKind) -> CacheWatch {
        let (sender, watch) = CacheWatch::channel(WATCH_CAPACITY);
        let mut guard = self.lock();
        let id = guard.next_watch_id;
        guard.next_watch_id += 1;
        guard.watchers.push(Watcher { id, kind, sender });
        watch
    }
}

/// The detached sweeper driving TTL expiry; exits once the cache is dropped.
async fn sweep_loop(weak: Weak<MemoryCache>) {
    let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
    loop {
        ticker.tick().await;
        let Some(cache) = weak.upgrade() else {
            return;
        };
        cache.sweep_expired().await;
    }
}

#[async_trait]
impl ClusterCacheBackend for MemoryCache {
    fn consistency(&self) -> CacheConsistency {
        self.consistency
    }

    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(!matches!(
            self.prefix_watch,
            PrefixWatchBehavior::Unsupported
        ))
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        let now = Instant::now();
        let guard = self.lock();
        Ok(match guard.map.get(key) {
            Some(stored) if !stored.is_expired(now) => Some(stored.entry()),
            _ => None,
        })
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        let key = req.key;
        let now = Instant::now();
        {
            let mut guard = self.lock();
            let version = match guard.map.get(key) {
                Some(stored) if !stored.is_expired(now) => stored.version + 1,
                _ => 1,
            };
            guard.map.insert(
                key.to_owned(),
                Stored {
                    value: req.value.to_vec(),
                    version,
                    expires_at: req.ttl.as_duration().map(|d| now + d),
                },
            );
        }
        self.broadcast(
            key,
            CacheEvent::Changed {
                key: key.to_owned(),
            },
        )
        .await;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        let now = Instant::now();
        let was_live = {
            let mut guard = self.lock();
            let live = matches!(guard.map.get(key), Some(stored) if !stored.is_expired(now));
            guard.map.remove(key);
            live
        };
        if was_live {
            self.broadcast(
                key,
                CacheEvent::Deleted {
                    key: key.to_owned(),
                },
            )
            .await;
        }
        Ok(was_live)
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        let now = Instant::now();
        let guard = self.lock();
        Ok(matches!(guard.map.get(key), Some(stored) if !stored.is_expired(now)))
    }

    async fn put_if_absent(&self, req: PutRequest<'_>) -> Result<Option<CacheEntry>, ClusterError> {
        let key = req.key;
        let now = Instant::now();
        let created = {
            let mut guard = self.lock();
            if matches!(guard.map.get(key), Some(stored) if !stored.is_expired(now)) {
                None
            } else {
                let stored = Stored {
                    value: req.value.to_vec(),
                    version: 1,
                    expires_at: req.ttl.as_duration().map(|d| now + d),
                };
                let entry = stored.entry();
                guard.map.insert(key.to_owned(), stored);
                Some(entry)
            }
        };
        if created.is_some() {
            self.broadcast(
                key,
                CacheEvent::Changed {
                    key: key.to_owned(),
                },
            )
            .await;
        }
        Ok(created)
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected_version: u64,
        new_value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        let now = Instant::now();
        let outcome = {
            let mut guard = self.lock();
            match guard.map.get(key) {
                Some(stored) if !stored.is_expired(now) => {
                    if stored.version == expected_version {
                        let version = stored.version + 1;
                        let stored = Stored {
                            value: new_value.to_vec(),
                            version,
                            expires_at: ttl.as_duration().map(|d| now + d),
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
                CacheEvent::Changed {
                    key: key.to_owned(),
                },
            )
            .await;
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
                CacheEvent::Deleted {
                    key: key.to_owned(),
                },
            )
            .await;
        }
        Ok(deleted)
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        match self.watch_behavior {
            WatchBehavior::Normal => Ok(self.register_watch(WatchKind::Exact(key.to_owned()))),
            WatchBehavior::DeadImmediate => {
                // Drop the sender at once: `recv` yields `None` on the first poll,
                // modelling a backend with an unusable watch.
                let (_sender, watch) = CacheWatch::channel(WATCH_CAPACITY);
                Ok(watch)
            }
            WatchBehavior::Rotating => {
                // Deliver no events, then drop the sender after a meaningful
                // interval so `recv` yields `None` — a legitimate rotation.
                let (sender, watch) = CacheWatch::channel(WATCH_CAPACITY);
                tokio::spawn(async move {
                    tokio::time::sleep(ROTATING_WATCH_LIFETIME).await;
                    drop(sender);
                });
                Ok(watch)
            }
        }
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        match self.prefix_watch {
            PrefixWatchBehavior::Native => {
                Ok(self.register_watch(WatchKind::Prefix(prefix.to_owned())))
            }
            PrefixWatchBehavior::Silent => {
                // A live-but-unregistered watch: the sender is parked in `Inner`
                // (so the stream stays open and the consumer's maintainer task does
                // not exit) but is not in `watchers`, so `broadcast` never selects
                // it and no event is ever delivered.
                let (sender, watch) = CacheWatch::channel(WATCH_CAPACITY);
                self.lock().parked_senders.push(sender);
                Ok(watch)
            }
            PrefixWatchBehavior::Unsupported => {
                if self.slow_unsupported_watch {
                    // Suspend so a concurrent caller can observe the in-progress
                    // maintainer mark before this attempt fails and rolls it back.
                    tokio::task::yield_now().await;
                }
                Err(ClusterError::Unsupported {
                    feature: "prefix_watch",
                })
            }
        }
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
#[path = "test_cache_tests.rs"]
mod test_cache_tests;
