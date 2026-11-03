//! One map of the proxy configuration cache: stored entries with their ID
//! and alias indexes, TTL expiry, a capacity limit that evicts the least
//! recently used entry, and the in-flight loads a request may join
//! (ADR-0020).
//!
//! A load runs as its own task and stores its own result, so it finishes,
//! stores and releases its database connection even when every request
//! waiting on it is cancelled. It stores only if no change detached it in
//! the meantime; errors reach every waiter and are never stored. A load
//! that panics or runs past [`LOAD_TIMEOUT`] answers an error, so its keys
//! load again on the next miss.

use std::any::Any;
use std::borrow::Borrow;
use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures_util::future::{self, BoxFuture, FutureExt, Shared};
use lru::LruCache;
use parking_lot::Mutex;
use tokio::time::Instant;
use tracing::{error, warn};
use uuid::Uuid;

use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::{CacheEviction, CacheLookup, CacheMap};
use crate::domain::repo::RepositoryError;

/// How long a load may run before its query is dropped and its waiters get
/// an error. Matches the pool's default acquire timeout: a load still
/// running then holds a connection and a hung query.
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// A section's key.
pub(super) trait SectionKey: Clone + Eq + Hash + Debug + Send + Sync + 'static {
    /// The alias the key is indexed under; `None` keeps it out of the alias
    /// index.
    fn alias(&self) -> Option<&str> {
        None
    }
}

/// A row a section stores, indexed by its ID so that a change to the row
/// finds every entry holding it.
pub(super) trait SectionValue: Send + Sync + 'static {
    fn id(&self) -> Uuid;
}

/// A load of one key: its rows.
pub(super) type LoadOne<V> = BoxFuture<'static, Result<Vec<V>, RepositoryError>>;
/// A batched load: each key it was asked for, with its rows.
pub(super) type LoadMany<K, V> = BoxFuture<'static, Result<Vec<(K, Vec<V>)>, RepositoryError>>;

/// A batched load's result, by key.
type Batch<K, V> = Result<Arc<HashMap<K, Arc<[V]>>>, RepositoryError>;
/// One key's share of an in-flight load.
type KeyLoad<V> = Shared<BoxFuture<'static, Result<Arc<[V]>, RepositoryError>>>;

/// What a change evicts from one section.
pub(super) enum Evict<K> {
    /// The change cannot affect the section: nothing is evicted or detached.
    Nothing,
    /// These entries, if stored; every in-flight load is detached.
    Keys(Vec<K>),
    /// Every entry; every in-flight load is detached.
    Everything,
}

/// One map of the cache.
pub(super) struct Section<K, V> {
    inner: Arc<Inner<K, V>>,
}

struct Inner<K, V> {
    /// A hit reorders the entries, so every lookup takes it exclusively.
    state: Mutex<State<K, V>>,
    ttl: Duration,
    max_entries: usize,
    /// The section's `map` label.
    map: CacheMap,
    metrics: Arc<dyn OagwMetricsPort>,
}

struct State<K, V> {
    /// Least recently used first. Unbounded: [`State::store`] enforces the
    /// capacity, so that it unlinks the index and counts the eviction. The
    /// std hasher, as keys come from requests.
    entries: LruCache<K, Entry<V>, RandomState>,
    /// Changes only together with `entries`, under the lock.
    index: Index<K>,
    /// In-flight loads a request may join. A change detaches them by
    /// clearing this map: a load stores its result only while its key still
    /// maps to it.
    loads: HashMap<K, Load<V>>,
    /// Source of load IDs.
    next_load: u64,
}

struct Entry<V> {
    value: Arc<[V]>,
    ids: Box<[Uuid]>,
    stored_at: Instant,
}

struct Load<V> {
    /// Shared by every key of one batched load.
    id: u64,
    fut: KeyLoad<V>,
}

/// The stored entries by the IDs their values hold, and by alias.
pub(super) struct Index<K> {
    by_id: HashMap<Uuid, HashSet<K>>,
    by_alias: HashMap<String, HashSet<K>>,
}

enum Slot<V> {
    Ready(Result<Arc<[V]>, RepositoryError>),
    Wait(KeyLoad<V>),
}

impl<K: SectionKey, V: SectionValue> Section<K, V> {
    /// A section whose entries live for `ttl`, holding at most
    /// `max_entries`, recording its metrics under the `map` label.
    pub(super) fn new(
        map: CacheMap,
        ttl: Duration,
        max_entries: usize,
        metrics: Arc<dyn OagwMetricsPort>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    entries: LruCache::unbounded_with_hasher(RandomState::new()),
                    index: Index::default(),
                    loads: HashMap::new(),
                    next_load: 0,
                }),
                ttl,
                max_entries,
                map,
                metrics,
            }),
        }
    }

    /// The value of `key`: the stored entry while fresh, else the in-flight
    /// load of the key, else a new load of it by `load`.
    pub(super) async fn get_or_load<F>(&self, key: K, load: F) -> Result<Arc<[V]>, RepositoryError>
    where
        F: FnOnce(K) -> LoadOne<V>,
    {
        let wanted = key.clone();
        let values = self
            .get_or_load_many(std::slice::from_ref(&wanted), move |_| {
                let rows = load(key.clone());
                async move { rows.await.map(|rows| vec![(key, rows)]) }.boxed()
            })
            .await?;
        // One key in, one value out.
        Ok(values.into_iter().next().unwrap_or_default())
    }

    /// The value of each of `keys`, in order: fresh entries are served,
    /// in-flight loads joined, and the rest loaded by one call of `load`.
    pub(super) async fn get_or_load_many<F>(
        &self,
        keys: &[K],
        load: F,
    ) -> Result<Vec<Arc<[V]>>, RepositoryError>
    where
        F: FnOnce(Vec<K>) -> LoadMany<K, V>,
    {
        let slots = self.slots(keys, load);
        future::try_join_all(slots.into_iter().map(|slot| async move {
            match slot {
                Slot::Ready(value) => value,
                Slot::Wait(load) => load.await,
            }
        }))
        .await
    }

    /// Evict what `select` picks. Unless it picks [`Evict::Nothing`], every
    /// in-flight load is detached in the same step: it still answers its
    /// waiters but is not stored.
    pub(super) fn evict(&self, select: impl FnOnce(&Index<K>) -> Evict<K>) {
        let mut state = self.inner.state.lock();
        let evicted = match select(&state.index) {
            Evict::Nothing => return,
            Evict::Keys(keys) => {
                state.detach_loads();
                keys.iter().filter(|key| state.remove(key)).count()
            }
            Evict::Everything => {
                let stored = state.entries.len();
                state.clear();
                stored
            }
        };
        self.inner.record_evictions(CacheEviction::Change, evicted);
        self.inner.record_entries(&state);
    }

    fn slots<F>(&self, keys: &[K], load: F) -> Vec<Slot<V>>
    where
        F: FnOnce(Vec<K>) -> LoadMany<K, V>,
    {
        let ttl = self.inner.ttl;
        let mut state = self.inner.state.lock();
        let now = Instant::now();
        let mut expired = Evicted::default();
        let hits: Vec<_> = keys
            .iter()
            .map(|key| state.lookup(key, now, ttl, &mut expired))
            .collect();
        if expired.expired > 0 {
            self.inner
                .record_evictions(CacheEviction::Ttl, expired.expired);
            self.inner.record_entries(&state);
        }

        let mut missing: Vec<K> = Vec::new();
        for (key, hit) in keys.iter().zip(&hits) {
            if hit.is_none() && !state.loads.contains_key(key) && !missing.contains(key) {
                missing.push(key.clone());
            }
        }
        if !missing.is_empty() {
            self.start_load(&mut state, missing.clone(), load);
        }
        keys.iter()
            .zip(hits)
            .map(|(key, hit)| match hit {
                Some(hit) => {
                    self.inner.record_lookup(CacheLookup::Hit);
                    Slot::Ready(Ok(hit))
                }
                None => state.loads.get(key).map_or_else(
                    || Slot::Ready(Err(internal("proxy cache key neither stored nor loading"))),
                    |load| {
                        self.inner.record_lookup(if missing.contains(key) {
                            CacheLookup::Miss
                        } else {
                            CacheLookup::Joined
                        });
                        Slot::Wait(load.fut.clone())
                    },
                ),
            })
            .collect()
    }

    /// Spawn one load of `missing` and register it for each key.
    fn start_load<F>(&self, state: &mut State<K, V>, missing: Vec<K>, load: F)
    where
        F: FnOnce(Vec<K>) -> LoadMany<K, V>,
    {
        let id = state.load_id();
        let inner = Arc::downgrade(&self.inner);
        let map = self.inner.map;
        let rows = load(missing.clone());
        let keys = missing.clone();
        // A panicking or hung load still finishes, so its keys do not stay
        // joinable.
        let task = tokio::spawn(async move {
            let loaded = tokio::time::timeout(LOAD_TIMEOUT, AssertUnwindSafe(rows).catch_unwind());
            let batch: Batch<K, V> = match loaded.await {
                Ok(Ok(rows)) => rows.map(|rows| {
                    Arc::new(
                        rows.into_iter()
                            .map(|(key, rows)| (key, Arc::from(rows)))
                            .collect(),
                    )
                }),
                // The keys are logged, not answered: an answer reaches API
                // error bodies.
                Ok(Err(panic)) => {
                    error!(
                        map = map.as_str(),
                        keys = keys.len(),
                        first = ?keys.first(),
                        panic = panic_message(panic.as_ref()),
                        "A proxy cache load panicked"
                    );
                    Err(internal(&format!(
                        "proxy cache load panicked (keys: {})",
                        keys.len()
                    )))
                }
                Err(_) => {
                    warn!(
                        map = map.as_str(),
                        keys = keys.len(),
                        first = ?keys.first(),
                        timeout = ?LOAD_TIMEOUT,
                        "A proxy cache load timed out"
                    );
                    Err(internal(&format!(
                        "proxy cache load timed out (keys: {})",
                        keys.len()
                    )))
                }
            };
            if let Some(inner) = Weak::upgrade(&inner) {
                inner.finish(&keys, id, batch.as_deref().ok());
            }
            batch
        });
        let batch = task
            .map(|joined| joined.unwrap_or_else(|_| Err(internal("proxy cache load was aborted"))))
            .boxed()
            .shared();

        for key in missing {
            let fut = batch
                .clone()
                .map({
                    let key = key.clone();
                    move |batch| {
                        batch?
                            .get(&key)
                            .cloned()
                            .ok_or_else(|| internal("proxy cache load returned no entry for a key"))
                    }
                })
                .boxed()
                .shared();
            state.loads.insert(key, Load { id, fut });
        }
    }
}

impl<K: SectionKey, V: SectionValue> Inner<K, V> {
    /// The end of load `id`: store its values if no change detached it.
    fn finish(&self, keys: &[K], id: u64, values: Option<&HashMap<K, Arc<[V]>>>) {
        let mut state = self.state.lock();
        let now = Instant::now();
        let mut evicted = Evicted::default();
        for key in keys {
            let value = values.and_then(|values| values.get(key));
            // Detached by a change, which cleared `loads`; a later load of
            // the key has its own ID.
            if state.loads.get(key).is_none_or(|load| load.id != id) {
                if value.is_some() {
                    self.metrics.record_proxy_cache_load_discarded(self.map);
                }
                continue;
            }
            state.loads.remove(key);
            if let Some(value) = value {
                let stored = state.store(
                    key.clone(),
                    Arc::clone(value),
                    now,
                    self.ttl,
                    self.max_entries,
                );
                evicted.expired += stored.expired;
                evicted.displaced += stored.displaced;
            }
        }
        self.record_evictions(CacheEviction::Ttl, evicted.expired);
        self.record_evictions(CacheEviction::Capacity, evicted.displaced);
        if values.is_some() {
            self.record_entries(&state);
        }
    }

    fn record_lookup(&self, result: CacheLookup) {
        self.metrics.record_proxy_cache_lookup(self.map, result);
    }

    fn record_evictions(&self, reason: CacheEviction, count: usize) {
        if count > 0 {
            self.metrics
                .record_proxy_cache_evictions(self.map, reason, count as u64);
        }
    }

    /// Called under the lock, so the last value recorded is the current one.
    fn record_entries(&self, state: &State<K, V>) {
        self.metrics
            .record_proxy_cache_entries(self.map, state.entries.len() as u64);
    }
}

/// The entries one store evicted.
#[derive(Default)]
struct Evicted {
    /// Older than the TTL.
    expired: usize,
    /// The least recently used, for room.
    displaced: usize,
}

impl<K: SectionKey, V: SectionValue> State<K, V> {
    /// The fresh value of `key`, which becomes the most recently used. An
    /// expired entry is removed and counted in `evicted`.
    fn lookup(
        &mut self,
        key: &K,
        now: Instant,
        ttl: Duration,
        evicted: &mut Evicted,
    ) -> Option<Arc<[V]>> {
        let entry = self.entries.get(key)?;
        if now.duration_since(entry.stored_at) < ttl {
            return Some(Arc::clone(&entry.value));
        }
        self.remove(key);
        evicted.expired += 1;
        None
    }

    fn load_id(&mut self) -> u64 {
        self.next_load += 1;
        self.next_load
    }

    /// Store `value` under `key` as the most recently used: expired entries
    /// at the least recently used end go first, then, when the section is
    /// full and `key` is new, the least recently used entry.
    ///
    /// An entry not used for a TTL has expired, so the expiry pass stops at
    /// the first fresh one. An expired entry used since goes on its next
    /// lookup, or when it reaches the end.
    fn store(
        &mut self,
        key: K,
        value: Arc<[V]>,
        now: Instant,
        ttl: Duration,
        max_entries: usize,
    ) -> Evicted {
        let mut evicted = Evicted::default();
        while let Some((_, entry)) = self.entries.peek_lru()
            && now.duration_since(entry.stored_at) >= ttl
            && self.pop_lru()
        {
            evicted.expired += 1;
        }
        if !self.remove(&key) && self.entries.len() >= max_entries && self.pop_lru() {
            evicted.displaced += 1;
        }

        let ids: Box<[Uuid]> = value.iter().map(SectionValue::id).collect();
        self.index.link(&key, &ids);
        self.entries.put(
            key,
            Entry {
                value,
                ids,
                stored_at: now,
            },
        );
        evicted
    }

    /// Remove the least recently used entry; `false` when there is none.
    fn pop_lru(&mut self) -> bool {
        let Some((key, entry)) = self.entries.pop_lru() else {
            return false;
        };
        self.index.unlink(&key, &entry.ids);
        true
    }

    fn remove(&mut self, key: &K) -> bool {
        let Some(entry) = self.entries.pop(key) else {
            return false;
        };
        self.index.unlink(key, &entry.ids);
        true
    }

    fn detach_loads(&mut self) {
        self.loads.clear();
    }

    fn clear(&mut self) {
        self.detach_loads();
        self.entries.clear();
        self.index = Index::default();
    }
}

impl<K: SectionKey> Index<K> {
    /// The keys whose stored value holds the row `id`.
    pub(super) fn holding(&self, id: Uuid) -> impl Iterator<Item = &K> {
        self.by_id.get(&id).into_iter().flatten()
    }

    /// The stored keys with `alias`.
    pub(super) fn with_alias(&self, alias: &str) -> impl Iterator<Item = &K> {
        self.by_alias.get(alias).into_iter().flatten()
    }

    fn link(&mut self, key: &K, ids: &[Uuid]) {
        for id in ids {
            self.by_id.entry(*id).or_default().insert(key.clone());
        }
        if let Some(alias) = key.alias() {
            self.by_alias
                .entry(alias.to_owned())
                .or_default()
                .insert(key.clone());
        }
    }

    fn unlink(&mut self, key: &K, ids: &[Uuid]) {
        for id in ids {
            unlink_from(&mut self.by_id, id, key);
        }
        if let Some(alias) = key.alias() {
            unlink_from(&mut self.by_alias, alias, key);
        }
    }
}

impl<K> Default for Index<K> {
    fn default() -> Self {
        Self {
            by_id: HashMap::new(),
            by_alias: HashMap::new(),
        }
    }
}

fn unlink_from<I, Q, K>(map: &mut HashMap<I, HashSet<K>>, at: &Q, key: &K)
where
    I: Borrow<Q> + Eq + Hash,
    Q: Eq + Hash + ?Sized,
    K: Eq + Hash,
{
    if let Some(keys) = map.get_mut(at) {
        keys.remove(key);
        if keys.is_empty() {
            map.remove(at);
        }
    }
}

fn internal(detail: &str) -> RepositoryError {
    RepositoryError::Internal(detail.to_owned())
}

/// The message a panic carried, when it is text.
fn panic_message(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("a non-text payload")
}

#[cfg(test)]
impl<K: SectionKey + std::fmt::Debug, V: SectionValue> Section<K, V> {
    /// Evict every entry and detach every in-flight load.
    pub(super) fn clear(&self) {
        self.inner.state.lock().clear();
    }

    /// Stored entries, fresh or not.
    pub(super) fn len(&self) -> usize {
        self.inner.state.lock().entries.len()
    }

    /// Whether `key` has a stored entry, fresh or not. Does not use it.
    pub(super) fn contains(&self, key: &K) -> bool {
        self.inner.state.lock().entries.contains(key)
    }

    /// In-flight loads a request could join.
    pub(super) fn loads(&self) -> usize {
        self.inner.state.lock().loads.len()
    }

    /// The indexes hold exactly the stored entries' keys, and the section is
    /// within its capacity.
    pub(super) fn assert_consistent(&self) {
        let state = self.inner.state.lock();
        assert!(
            state.entries.len() <= self.inner.max_entries,
            "{} entries over a capacity of {}",
            state.entries.len(),
            self.inner.max_entries
        );
        let mut index = Index::default();
        for (key, entry) in &state.entries {
            index.link(key, &entry.ids);
        }
        assert_eq!(state.index.by_id, index.by_id, "ID index");
        assert_eq!(state.index.by_alias, index.by_alias, "alias index");
    }
}

#[cfg(test)]
#[path = "section_tests.rs"]
mod tests;
