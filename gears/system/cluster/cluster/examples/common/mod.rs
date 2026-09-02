//! Shared fixtures for the cluster-SDK showcase examples.
//!
//! # Fixture — NOT a production backend
//!
//! [`MemCacheBackend`] is a functional, single-process [`ClusterCacheBackend`]
//! that lets every showcase example run with no external infrastructure (no
//! Postgres/Redis/K8s). It mirrors the contract smoke-test stub
//! (`tests/common`): one state map behind one mutex, one monotonic per-key
//! version source, one ordered channel per watcher, and a background sweeper
//! that expires TTL'd entries. It makes no attempt at the durability or
//! partition tolerance a real backend needs — production backends ship as
//! separate plugin crates (DECOMPOSITION out-of-scope follow-ups).
//!
//! [`wire`] runs the real [`ClusterWiring`] over these fixtures, which
//! makes a profile resolvable: since item `K4` a facade binds through the
//! process's single `dyn ClusterClient`, and the wiring is what registers it.
//! [`cache_profile`] shows the "implement cache only, get all three primitives"
//! guarantee — one cache, and the omit-default auto-wrap supplies the rest.
//!
//! This module lives under `examples/common/` (a subdirectory), so Cargo treats
//! it as a shared module included via `mod common;` rather than as a standalone
//! example binary.

// Each example includes this module but exercises only the surface it needs, so
// the unused remainder would otherwise trip `dead_code`.
#![allow(
    dead_code,
    reason = "each example binary includes this module but uses only a subset of the fixture surface"
)]

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use cluster::defaults::{CasBasedDistributedLockBackend, CasBasedLeaderElectionBackend};
use cluster::{ClusterHandle, ClusterWiring, ProfileBackends};
use cluster_sdk::cache::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, CacheWatch, CacheWatchEvent,
    CacheWatchSender, ClusterCacheBackend, PutRequest, Ttl,
};
use cluster_sdk::error::ClusterError;
use parking_lot::Mutex;
use tokio::time::Instant;
use toolkit::client_hub::ClientHub;

/// How often the background sweeper scans for expired entries.
const SWEEP_INTERVAL: Duration = Duration::from_millis(25);

/// Per-watch in-flight buffer; generous so example workloads never drop events.
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
}

/// The fixture's locked interior: the state map, the monotonic watch-id source,
/// and the live watchers.
struct Inner {
    map: HashMap<String, Stored>,
    watchers: Vec<Watcher>,
    next_watch_id: u64,
}

/// A functional in-memory cache backend for the showcase examples.
///
/// See the module docs: this is a fixture, not a production backend.
pub struct MemCacheBackend {
    inner: Mutex<Inner>,
    consistency: CacheConsistency,
    prefix_watch: bool,
}

impl MemCacheBackend {
    /// A linearizable cache with native prefix-watch support — the common case
    /// that satisfies the consistency-sensitive default backends.
    #[must_use]
    pub fn linearizable() -> Arc<Self> {
        Self::spawn(CacheConsistency::Linearizable, true)
    }

    /// An eventually-consistent cache, used to show a capability mismatch
    /// (`CacheCapability::Linearizable` unmet) and the default-backend guard.
    #[must_use]
    pub fn eventually_consistent() -> Arc<Self> {
        Self::spawn(CacheConsistency::EventuallyConsistent, true)
    }

    /// A linearizable cache that declares no native prefix watch, so
    /// `watch_prefix` is unsupported and the polling polyfill is needed.
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
        // The sweeper holds only a weak reference, so it self-terminates once the
        // example drops the cache.
        let weak = Arc::downgrade(&cache);
        tokio::spawn(sweep_loop(weak));
        cache
    }

    /// Sends `event` to every watcher matching `key`, pruning any whose consumer
    /// has dropped the watch. The guard is released before any `.await`.
    async fn broadcast(&self, key: &str, event: &CacheEvent) {
        let targets: Vec<(u64, CacheWatchSender)> = {
            let guard = self.inner.lock();
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
            self.inner
                .lock()
                .watchers
                .retain(|watcher| !dead.contains(&watcher.id));
        }
    }

    /// Removes every expired entry and emits an `Expired` event for each.
    async fn sweep_expired(&self) {
        let now = Instant::now();
        let expired: Vec<String> = {
            let mut guard = self.inner.lock();
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
            self.broadcast(&key, &CacheEvent::Expired { key: key.clone() })
                .await;
        }
    }

    fn register_watch(&self, kind: WatchKind) -> CacheWatch {
        let (sender, watch) = CacheWatch::channel(WATCH_CAPACITY);
        let mut guard = self.inner.lock();
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
        let guard = self.inner.lock();
        Ok(match guard.map.get(key) {
            Some(stored) if !stored.is_expired(now) => Some(stored.entry()),
            _ => None,
        })
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        let now = Instant::now();
        let key = req.key;
        {
            let mut guard = self.inner.lock();
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
            &CacheEvent::Changed {
                key: key.to_owned(),
            },
        )
        .await;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        let now = Instant::now();
        let was_live = {
            let mut guard = self.inner.lock();
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
            )
            .await;
        }
        Ok(was_live)
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        let now = Instant::now();
        let guard = self.inner.lock();
        Ok(matches!(guard.map.get(key), Some(stored) if !stored.is_expired(now)))
    }

    async fn put_if_absent(&self, req: PutRequest<'_>) -> Result<Option<CacheEntry>, ClusterError> {
        let now = Instant::now();
        let key = req.key;
        let created = {
            let mut guard = self.inner.lock();
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
                &CacheEvent::Changed {
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
            let mut guard = self.inner.lock();
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
                &CacheEvent::Changed {
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
        let guard = self.inner.lock();
        Ok(guard
            .map
            .iter()
            .filter(|(key, stored)| key.starts_with(prefix) && !stored.is_expired(now))
            .map(|(key, _)| key.clone())
            .collect())
    }
}

/// One profile's bindings from a cache alone — the "implement cache only, get all
/// three primitives" composition, as [`ClusterWiring`] performs it from operator
/// config.
///
/// A linearizable cache needs nothing but itself: the omit-default auto-wrap
/// supplies leader election and the lock over it. An **eventually-consistent**
/// cache is a different matter — the two defaults reject one, which is the
/// consistency guard doing its job — so this fixture binds them explicitly
/// through the weak-consistency escape hatch. That keeps the guard honest in
/// production while letting the examples that only exercise the *cache* run
/// against a weaker one.
pub fn cache_profile(cache: Arc<dyn ClusterCacheBackend>) -> ProfileBackends {
    if cache.consistency() == CacheConsistency::Linearizable {
        return ProfileBackends::new(cache);
    }
    // The lease backends get a `reserved_lease_cache` view, never the handle the
    // cache API serves — the two keyspaces are physically one store, so sharing
    // the raw handle makes the lease records reachable through the cache API.
    // This mirrors the production wiring; an example must not demonstrate the
    // aliased construction.
    let lease_cache = cluster_sdk::reserved_lease_cache(Arc::clone(&cache));
    let leader =
        CasBasedLeaderElectionBackend::new_allow_weak_consistency(Arc::clone(&lease_cache));
    let lock = CasBasedDistributedLockBackend::new_allow_weak_consistency(Arc::clone(&lease_cache));
    ProfileBackends::new(cache)
        .with_leader_election(Arc::new(leader))
        .with_lock(Arc::new(lock))
}

/// Wires `profiles` through the real [`ClusterWiring`] and makes them resolvable
/// in `hub`.
///
/// A facade resolves through the process's single `dyn ClusterClient`
/// (DESIGN.md), which the wiring registers over the
/// profile set it published — so binding a backend into the hub is not by itself
/// enough for a consumer to reach it, and this is the one call that makes a
/// profile usable.
///
/// The returned handle must be stopped before the process exits; dropping one
/// unstopped is a programming error the handle reports loudly (ADR-006).
///
/// # Errors
/// Propagates whatever the wiring rejects — an invalid profile name, or a default
/// backend that refuses its cache's consistency class.
pub fn wire(
    hub: &Arc<ClientHub>,
    profiles: Vec<(&'static str, ProfileBackends)>,
) -> Result<ClusterHandle, ClusterError> {
    let mut builder = ClusterWiring::builder(Arc::clone(hub));
    for (name, backends) in profiles {
        builder = builder.profile_named(name, backends);
    }
    builder.build_and_start()
}
