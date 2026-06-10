// Created: 2026-06-11 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-showcase-audit-examples:p1
// @cpt-dod:cpt-cf-clst-dod-showcase-audit-traceability:p1
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
//! [`register_cache_and_siblings`] shows the "implement cache only, get all four
//! primitives" guarantee: it registers the cache plus the three SDK default
//! backends (`CasBased*` / `CacheBased*`) under one profile, exactly as the
//! follow-up wiring crate will.
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
use cluster_sdk::cache::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, CacheWatch, CacheWatchEvent,
    CacheWatchSender, ClusterCacheBackend,
};
use cluster_sdk::error::ClusterError;
use cluster_sdk::registration::{
    register_cache_backend, register_leader_election_backend, register_lock_backend,
    register_service_discovery_backend,
};
use cluster_sdk::{
    CacheBasedServiceDiscoveryBackend, CasBasedDistributedLockBackend,
    CasBasedLeaderElectionBackend,
};
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
// @cpt-begin:cpt-cf-clst-algo-showcase-audit-traceability:p1:inst-ta-foreach
// @cpt-begin:cpt-cf-clst-algo-showcase-audit-traceability:p1:inst-ta-map
// @cpt-begin:cpt-cf-clst-algo-showcase-audit-traceability:p1:inst-ta-markers
// @cpt-begin:cpt-cf-clst-algo-showcase-audit-traceability:p1:inst-ta-openq
// @cpt-begin:cpt-cf-clst-algo-showcase-audit-traceability:p1:inst-ta-return
pub struct MemCacheBackend {
    inner: Mutex<Inner>,
    consistency: CacheConsistency,
    prefix_watch: bool,
}
// @cpt-end:cpt-cf-clst-algo-showcase-audit-traceability:p1:inst-ta-return
// @cpt-end:cpt-cf-clst-algo-showcase-audit-traceability:p1:inst-ta-openq
// @cpt-end:cpt-cf-clst-algo-showcase-audit-traceability:p1:inst-ta-markers
// @cpt-end:cpt-cf-clst-algo-showcase-audit-traceability:p1:inst-ta-map
// @cpt-end:cpt-cf-clst-algo-showcase-audit-traceability:p1:inst-ta-foreach

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

    async fn put(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<(), ClusterError> {
        let now = Instant::now();
        {
            let mut guard = self.inner.lock();
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

    async fn put_if_absent(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        let now = Instant::now();
        let created = {
            let mut guard = self.inner.lock();
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
        ttl: Option<Duration>,
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

/// Registers a cache backend under `profile_name` together with the three SDK
/// default backends derived from it — leader election, distributed lock, and
/// service discovery — so all four primitives resolve against one cache.
///
/// This is exactly the "implement cache only, get all four primitives"
/// composition the follow-up wiring crate performs from operator config.
///
/// # Errors
/// Returns [`ClusterError::InvalidConfig`] if the consistency-sensitive default
/// backends reject the cache's consistency class, or any registration error
/// from the [`ClientHub`].
pub fn register_cache_and_siblings(
    hub: &ClientHub,
    profile_name: &'static str,
    cache: Arc<dyn ClusterCacheBackend>,
) -> Result<(), ClusterError> {
    // The two consistency-sensitive defaults reject an eventually-consistent
    // cache via `new()`; over a linearizable cache they construct cleanly.
    let leader = CasBasedLeaderElectionBackend::new(Arc::clone(&cache))?;
    let lock = CasBasedDistributedLockBackend::new(Arc::clone(&cache))?;
    let discovery = CacheBasedServiceDiscoveryBackend::new(Arc::clone(&cache));

    register_cache_backend(hub, profile_name, cache)?;
    register_leader_election_backend(hub, profile_name, Arc::new(leader))?;
    register_lock_backend(hub, profile_name, Arc::new(lock))?;
    register_service_discovery_backend(hub, profile_name, Arc::new(discovery))?;
    Ok(())
}
