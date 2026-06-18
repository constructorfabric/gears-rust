// Created: 2026-06-10 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-smoke-tests-stubs:p1
//! Minimal in-process stub backends for the cluster SDK contract smoke tests
//! (`cpt-cf-clst-dod-smoke-tests-stubs`, `cpt-cf-clst-algo-smoke-tests-stub-model`).
//!
//! # Contract fixture — NOT a production backend (`inst-sm-fixture`)
//!
//! [`MemCacheBackend`] is a functional, single-process [`ClusterCacheBackend`]
//! used **solely** to exercise the public SDK contract end-to-end with no
//! external infrastructure (no Postgres/Redis/K8s). It is deliberately simple —
//! one state map behind one mutex, one monotonic per-key version source
//! (`inst-sm-state`), one ordered channel per watcher so per-key ordering is
//! observable (`inst-sm-channel`) — and makes no attempt at the durability,
//! partition tolerance, or broadcast fan-out a real backend needs. Per-plugin
//! integration tests verify distributed correctness; these stubs verify API
//! shape and the happy-and-error paths the contract guarantees.
//!
//! The three remaining primitives are obtained as the "siblings" of this cache
//! through the SDK default backends (`CasBased*` / `CacheBased*`), which is the
//! whole point of the "implement cache only, get all four primitives" guarantee
//! — so the smoke tests need only this one hand-written stub.

#![allow(
    dead_code,
    reason = "each integration-test binary includes this module but uses only a subset of the fixture surface"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::cache::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, CacheWatch, CacheWatchEvent,
    CacheWatchSender, ClusterCacheBackend, PutRequest, Ttl,
};
use cluster_sdk::error::ClusterError;
use cluster_sdk::profile::ClusterProfile;
use tokio::time::Instant;

/// The typed profile every smoke test binds its backends under. A single shared
/// profile keeps the register/resolve round-trip uniform across the suite.
#[derive(Clone, Copy)]
pub struct SmokeProfile;

impl ClusterProfile for SmokeProfile {
    const NAME: &'static str = "smoke";
}

/// How often the background sweeper scans for expired entries. Fine enough that
/// a `tokio::time::advance` past a TTL deterministically triggers an `Expired`
/// emission (`inst-sm-signals`), coarse enough to avoid needless wakeups.
const SWEEP_INTERVAL: Duration = Duration::from_millis(25);

/// Per-watch in-flight buffer. Generous so a renewal/heartbeat storm in a test
/// never spuriously drops events.
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
// @cpt-begin:cpt-cf-clst-algo-smoke-tests-stub-model:p1:inst-sm-channel
struct Watcher {
    id: u64,
    kind: WatchKind,
    sender: CacheWatchSender,
}
// @cpt-end:cpt-cf-clst-algo-smoke-tests-stub-model:p1:inst-sm-channel

/// The fixture's locked interior: a single state map and a single monotonic
/// version source (`inst-sm-state`), plus the live watchers.
// @cpt-begin:cpt-cf-clst-algo-smoke-tests-stub-model:p1:inst-sm-state
struct Inner {
    map: HashMap<String, Stored>,
    watchers: Vec<Watcher>,
    next_watch_id: u64,
}
// @cpt-end:cpt-cf-clst-algo-smoke-tests-stub-model:p1:inst-sm-state

/// A functional in-memory cache backend for the contract smoke tests.
///
/// See the module docs: this is a fixture, not a production backend.
// @cpt-begin:cpt-cf-clst-algo-smoke-tests-stub-model:p1:inst-sm-fixture
pub struct MemCacheBackend {
    inner: Mutex<Inner>,
    consistency: CacheConsistency,
    prefix_watch: bool,
}
// @cpt-end:cpt-cf-clst-algo-smoke-tests-stub-model:p1:inst-sm-fixture

impl MemCacheBackend {
    /// A linearizable cache with native prefix-watch support — the common case
    /// that satisfies the consistency-sensitive default backends.
    #[must_use]
    pub fn linearizable() -> Arc<Self> {
        Self::spawn(CacheConsistency::Linearizable, true)
    }

    /// An eventually-consistent cache, for exercising a capability mismatch
    /// (`CacheCapability::Linearizable` unmet) and the default-backend
    /// constructor guard.
    #[must_use]
    pub fn eventually_consistent() -> Arc<Self> {
        Self::spawn(CacheConsistency::EventuallyConsistent, true)
    }

    /// A linearizable cache that declares no native prefix watch, so
    /// `watch_prefix` returns [`ClusterError::Unsupported`] — for the
    /// `CacheCapability::PrefixWatch` mismatch and the polling polyfill.
    #[must_use]
    pub fn linearizable_without_prefix_watch() -> Arc<Self> {
        Self::spawn(CacheConsistency::Linearizable, false)
    }

    fn spawn(consistency: CacheConsistency, prefix_watch: bool) -> Arc<Self> {
        let cache = Arc::new(Self {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                watchers: Vec::new(),
                next_watch_id: 0,
            }),
            consistency,
            prefix_watch,
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

    /// Removes every expired entry and emits an `Expired` event for each
    /// (`inst-sm-signals`).
    async fn sweep_expired(&self) {
        // @cpt-begin:cpt-cf-clst-algo-smoke-tests-stub-model:p1:inst-sm-signals
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
        // @cpt-end:cpt-cf-clst-algo-smoke-tests-stub-model:p1:inst-sm-signals
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
async fn sweep_loop(weak: Weak<MemCacheBackend>) {
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
impl ClusterCacheBackend for MemCacheBackend {
    fn consistency(&self) -> CacheConsistency {
        self.consistency
    }

    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(self.prefix_watch)
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

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        Ok(self.register_watch(WatchKind::Exact(key.to_owned())))
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        if !self.prefix_watch {
            return Err(ClusterError::Unsupported {
                feature: "prefix_watch",
            });
        }
        Ok(self.register_watch(WatchKind::Prefix(prefix.to_owned())))
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
