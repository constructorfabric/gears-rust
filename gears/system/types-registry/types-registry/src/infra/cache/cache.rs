//! Cache infrastructure for [`TypesRegistryLocalClient`](crate::domain::legacy_local_client::TypesRegistryLocalClient).
//!
//! Provides bounded LRU caches for resolved [`GtsTypeSchema`] and [`GtsInstance`]
//! values, keyed by GTS id. Both kinds share a generic [`Cache<V>`] backbone.
//!
//! # TTL
//!
//! TTL is enabled by default ([`DEFAULT_CACHE_TTL`]). The local client
//! invalidates its own entries on writes it observes, but registry mutations
//! can also reach the underlying store from other processes (out-of-process
//! gears) or from peers in a future distributed deployment — there's no
//! in-process notification when that happens. TTL bounds how long a stale
//! entry can survive in those cases. Callers can override with
//! [`CacheConfig::with_ttl`] / [`CacheConfig::without_ttl`].
//!
//! # Lock ordering
//!
//! Each cache owns one `parking_lot::Mutex`. The local client holds at most
//! one cache lock at a time and never acquires the storage repository lock
//! while holding a cache lock.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lru::LruCache;
use parking_lot::Mutex;
use toolkit_macros::domain_model;
use types_registry_sdk::{GtsInstance, GtsTypeSchema};
use uuid::Uuid;

/// Provides the deterministic UUID v5 of an entity for indexing inside the
/// cache.
///
/// Both [`GtsTypeSchema`] (with `type_uuid`) and [`GtsInstance`] (with `uuid`)
/// carry a UUID derived from the GTS id, but under different field names.
/// This trait gives [`InMemoryCache`] a uniform extractor so it can populate
/// its internal `uuid → gts_id` reverse index atomically with every `put`.
///
/// Kept private to the cache gear — it's an impl detail of
/// [`InMemoryCache`], not part of the SDK contract.
pub trait HasUuid {
    /// Returns the entity's deterministic UUID v5.
    fn entity_uuid(&self) -> Uuid;
}

impl HasUuid for GtsTypeSchema {
    fn entity_uuid(&self) -> Uuid {
        self.type_uuid
    }
}

impl HasUuid for GtsInstance {
    fn entity_uuid(&self) -> Uuid {
        self.uuid
    }
}

impl<T: HasUuid + ?Sized> HasUuid for Arc<T> {
    fn entity_uuid(&self) -> Uuid {
        T::entity_uuid(self)
    }
}

/// Default cache capacity (entries) for both type-schema and instance caches.
pub const DEFAULT_CACHE_CAPACITY: usize = 1024;

/// Default TTL for cached entries: 60 seconds.
///
/// Bounds staleness when the underlying store is mutated outside the local
/// client's awareness (e.g. by an out-of-process gear sharing the registry
/// or a peer node in a future distributed deployment).
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_mins(1);

/// Per-kind cache configuration.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct CacheConfig {
    /// Maximum number of entries before LRU eviction. Clamped to `1` if `0`.
    pub capacity: usize,
    /// Maximum age of an entry before it's treated as a miss. `None` disables
    /// TTL entirely (entries live until evicted by capacity pressure).
    pub ttl: Option<Duration>,
}

impl CacheConfig {
    /// Default config for the type-schema cache: [`DEFAULT_CACHE_CAPACITY`]
    /// entries, [`DEFAULT_CACHE_TTL`].
    #[must_use]
    pub const fn type_schemas() -> Self {
        Self {
            capacity: DEFAULT_CACHE_CAPACITY,
            ttl: Some(DEFAULT_CACHE_TTL),
        }
    }

    /// Default config for the instance cache: [`DEFAULT_CACHE_CAPACITY`]
    /// entries, [`DEFAULT_CACHE_TTL`].
    #[must_use]
    pub const fn instances() -> Self {
        Self {
            capacity: DEFAULT_CACHE_CAPACITY,
            ttl: Some(DEFAULT_CACHE_TTL),
        }
    }

    /// Builder: set capacity.
    #[must_use]
    pub const fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    /// Builder: set TTL.
    #[must_use]
    pub const fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Builder: disable TTL.
    #[must_use]
    pub const fn without_ttl(mut self) -> Self {
        self.ttl = None;
        self
    }
}

#[domain_model]
#[derive(Debug, Clone)]
struct Entry<V> {
    value: V,
    inserted: Instant,
}

/// Abstract cache contract used by [`TypesRegistryLocalClient`](crate::domain::legacy_local_client::TypesRegistryLocalClient).
///
/// Designed for swappable implementations: today the registry runs entirely
/// in-process with [`InMemoryCache`], tomorrow we may add a Redis-backed
/// implementation for sharing cache state across pods. All methods take
/// `&self` (interior mutability inside the impl) and are sync — async
/// implementations should buffer batches behind a sync façade.
///
/// In addition to the LRU value cache, every implementation must maintain a
/// reverse `uuid → gts_id` index populated atomically on `put` /
/// `put_many` and pruned in lock-step on every removal — capacity-driven
/// LRU eviction, TTL expiry inside `get*`, [`Self::invalidate`],
/// [`Self::retain`], and [`Self::clear`]. The index size therefore tracks
/// the LRU's. The fast paths into the index are [`Self::get_by_uuid`] /
/// [`Self::get_many_by_uuid`].
pub trait Cache<V>: Send + Sync
where
    V: Clone + HasUuid,
{
    /// Looks up an entry. `None` if absent or expired.
    fn get(&self, key: &str) -> Option<V>;

    /// Bulk lookup with single-acquisition semantics. Returns one entry per
    /// input key in the same order, with `None` for misses.
    fn get_many(&self, keys: &[&str]) -> Vec<Option<V>>;

    /// Looks up a cached value by its UUID v5. Returns `Some(value)` only
    /// when the UUID has been observed (via `put` / `put_many`) **and** the
    /// resolved LRU entry is still present and not expired. A
    /// previously-observed UUID whose value has been evicted by LRU
    /// capacity or TTL returns `None` — callers fall through to their
    /// usual slow path, which will re-cache and re-index on the way back.
    fn get_by_uuid(&self, uuid: Uuid) -> Option<V>;

    /// Bulk variant of [`Self::get_by_uuid`] under a single lock.
    fn get_many_by_uuid(&self, uuids: &[Uuid]) -> Vec<Option<V>>;

    /// Inserts (or replaces) an entry. Atomically records the
    /// `value.entity_uuid() → key` mapping in the reverse index.
    fn put(&self, key: String, value: V);

    /// Bulk insert with single-acquisition semantics. All entries enter the
    /// cache as if put together (shared TTL clock if applicable). Atomically
    /// records every `value.entity_uuid() → key` mapping in the reverse
    /// index.
    fn put_many(&self, entries: Vec<(String, V)>);

    /// Removes a single entry. No-op if absent. The matching `uuid → gts_id`
    /// mapping is pruned along with the LRU entry.
    fn invalidate(&self, key: &str);

    /// Drops every entry whose value fails the predicate. Used for cascade
    /// invalidation. Object-safe form: takes `&dyn Fn` instead of generic
    /// `F: Fn`. Reverse `uuid → gts_id` mappings of removed entries are
    /// pruned in lock-step.
    fn retain(&self, predicate: &dyn Fn(&V) -> bool);

    /// Drops every entry, including the `uuid → gts_id` index.
    fn clear(&self);

    /// Number of entries currently held (including not-yet-expired stale ones).
    fn len(&self) -> usize;

    /// Returns `true` if the cache has no entries.
    fn is_empty(&self) -> bool;
}

/// LRU + reverse `uuid → gts_id` index, both behind one mutex.
///
/// Wrapping these together (rather than two independent `Mutex`es) gives
/// every `put` true atomicity — readers either see both maps populated or
/// neither, never an in-between state where the index points at an entry
/// the LRU hasn't received yet. Every method that mutates the LRU also
/// prunes the matching `uuid_to_id` entry (see [`Inner::pop_with_cleanup`]
/// / [`Inner::push_with_cleanup`]) so the index stays bounded by the LRU
/// capacity rather than growing for the lifetime of the process.
struct Inner<V> {
    lru: LruCache<String, Entry<V>>,
    /// Reverse `uuid → gts_id` index. Kept in sync with the LRU so its
    /// memory footprint matches the LRU's: every entry that leaves the
    /// LRU (capacity eviction, TTL expiry, invalidate, retain) also
    /// drops here.
    uuid_to_id: HashMap<Uuid, String>,
}

impl<V: Clone + HasUuid> Inner<V> {
    /// Pops the LRU entry for `key` and removes its `uuid → key` mapping
    /// from the reverse index. Keeps the two maps in lock-step.
    fn pop_with_cleanup(&mut self, key: &str) -> Option<Entry<V>> {
        let entry = self.lru.pop(key)?;
        self.uuid_to_id.remove(&entry.value.entity_uuid());
        Some(entry)
    }

    /// Inserts `entry` at `key`, replacing any existing entry. If the
    /// insert evicts another entry (capacity-driven eviction or same-key
    /// replace), prunes the evicted entry's `uuid → key` mapping before
    /// recording the new one.
    fn push_with_cleanup(&mut self, key: String, entry: Entry<V>) {
        let new_uuid = entry.value.entity_uuid();
        if let Some((_, evicted)) = self.lru.push(key.clone(), entry) {
            self.uuid_to_id.remove(&evicted.value.entity_uuid());
        }
        // Insert AFTER the cleanup: in the deterministic case
        // (same-key replace where new and old uuids are equal) the
        // cleanup above removed our mapping, so we restore it here.
        self.uuid_to_id.insert(new_uuid, key);
    }
}

/// Bounded LRU cache with optional TTL, keyed by GTS id `String`. Every
/// stored value also feeds a reverse `uuid → gts_id` index (built from
/// [`HasUuid::entity_uuid`]) so `*_by_uuid` lookups can short-circuit the
/// linear storage scan once a UUID has been observed.
///
/// Cache hits return cloned values. For `Arc<T>` this is a refcount bump.
#[domain_model]
pub struct InMemoryCache<V: Clone + HasUuid> {
    inner: Mutex<Inner<V>>,
    ttl: Option<Duration>,
}

impl<V: Clone + HasUuid> InMemoryCache<V> {
    /// Creates a new cache with the given config.
    ///
    /// `config.capacity == 0` is silently clamped to `1`.
    #[must_use]
    pub fn new(config: CacheConfig) -> Self {
        let capacity = NonZeroUsize::new(config.capacity).unwrap_or(NonZeroUsize::MIN);
        Self {
            inner: Mutex::new(Inner {
                lru: LruCache::new(capacity),
                uuid_to_id: HashMap::new(),
            }),
            ttl: config.ttl,
        }
    }

    /// Looks up an entry. Returns `None` if absent or expired (and removes the
    /// expired entry as a side effect).
    pub fn get(&self, key: &str) -> Option<V> {
        let mut guard = self.inner.lock();
        let entry = guard.lru.get(key)?;
        if let Some(ttl) = self.ttl
            && entry.inserted.elapsed() > ttl
        {
            guard.pop_with_cleanup(key);
            return None;
        }
        Some(entry.value.clone())
    }

    /// Bulk lookup: acquires the lock once for the whole batch and returns
    /// one entry per input key, in the same order.
    ///
    /// Per-key semantics match [`Self::get`]: a hit refreshes LRU recency,
    /// expired entries are evicted as a side effect (deferred until the
    /// end of the batch so we don't churn the lock state mid-iteration).
    ///
    /// Designed for upcoming database-backed implementations where issuing
    /// one query for many keys (e.g. `WHERE gts_id IN (...)`) is dramatically
    /// cheaper than N round-trips, and for amortizing the in-memory mutex
    /// across batch fast-paths in clients.
    pub fn get_many(&self, keys: &[&str]) -> Vec<Option<V>> {
        let mut guard = self.inner.lock();
        let mut to_evict: Vec<String> = Vec::new();
        let mut results: Vec<Option<V>> = Vec::with_capacity(keys.len());
        for key in keys {
            match guard.lru.get(*key) {
                Some(entry) => {
                    let expired = self.ttl.is_some_and(|ttl| entry.inserted.elapsed() > ttl);
                    if expired {
                        to_evict.push((*key).to_owned());
                        results.push(None);
                    } else {
                        results.push(Some(entry.value.clone()));
                    }
                }
                None => results.push(None),
            }
        }
        for key in &to_evict {
            guard.pop_with_cleanup(key.as_str());
        }
        results
    }

    /// Inserts or replaces an entry. Resets the TTL clock for the key, and
    /// atomically records the `value.entity_uuid() → key` mapping in the
    /// reverse `uuid → gts_id` index (same lock as the LRU). If the put
    /// triggers capacity eviction, the evicted entry's reverse mapping is
    /// also dropped.
    pub fn put(&self, key: String, value: V) {
        let mut guard = self.inner.lock();
        guard.push_with_cleanup(
            key,
            Entry {
                value,
                inserted: Instant::now(),
            },
        );
    }

    /// Bulk insert: all entries are written under a single lock acquisition
    /// and share the same TTL clock origin. Existing entries with the same
    /// key are replaced. Every value's `entity_uuid() → key` mapping is
    /// recorded in the reverse `uuid → gts_id` index, and any entries
    /// evicted by capacity pressure during the batch have their mappings
    /// pruned in lock-step.
    ///
    /// Designed for batch fast-paths in clients: when a `get_*` batch fills
    /// in N cache misses by fetching from storage, those N freshly-built
    /// entries can be installed in one shot rather than N individual `put`s.
    pub fn put_many(&self, entries: Vec<(String, V)>) {
        let mut guard = self.inner.lock();
        let now = Instant::now();
        for (key, value) in entries {
            guard.push_with_cleanup(
                key,
                Entry {
                    value,
                    inserted: now,
                },
            );
        }
    }

    /// Looks up a cached value by its UUID v5. Returns `Some(value)` only
    /// when the UUID has been observed and the LRU entry is still present
    /// and unexpired. A reverse-index hit followed by an LRU miss yields
    /// `None`; callers handle it like any other cache miss.
    pub fn get_by_uuid(&self, uuid: Uuid) -> Option<V> {
        let mut guard = self.inner.lock();
        let id = guard.uuid_to_id.get(&uuid).cloned()?;
        let entry = guard.lru.get(id.as_str())?;
        if let Some(ttl) = self.ttl
            && entry.inserted.elapsed() > ttl
        {
            guard.pop_with_cleanup(id.as_str());
            return None;
        }
        Some(entry.value.clone())
    }

    /// Bulk variant of [`Self::get_by_uuid`] — single lock acquisition for
    /// the whole batch. Returns one entry per input UUID, in input order.
    pub fn get_many_by_uuid(&self, uuids: &[Uuid]) -> Vec<Option<V>> {
        let mut guard = self.inner.lock();
        let mut to_evict: Vec<String> = Vec::new();
        let mut results: Vec<Option<V>> = Vec::with_capacity(uuids.len());
        for uuid in uuids {
            let Some(id) = guard.uuid_to_id.get(uuid).cloned() else {
                results.push(None);
                continue;
            };
            match guard.lru.get(id.as_str()) {
                Some(entry) => {
                    let expired = self.ttl.is_some_and(|ttl| entry.inserted.elapsed() > ttl);
                    if expired {
                        to_evict.push(id);
                        results.push(None);
                    } else {
                        results.push(Some(entry.value.clone()));
                    }
                }
                None => results.push(None),
            }
        }
        for id in &to_evict {
            guard.pop_with_cleanup(id.as_str());
        }
        results
    }

    /// Removes a single entry by key. No-op if absent. The matching
    /// `uuid → gts_id` mapping is dropped in lock-step.
    pub fn invalidate(&self, key: &str) {
        self.inner.lock().pop_with_cleanup(key);
    }

    /// Drops every entry whose value fails the predicate. Used for cascade
    /// invalidation when a parent type-schema is rewritten — derived
    /// type-schemas and instances embedding `Arc`s to the old parent must
    /// be dropped, otherwise reads return stale views. The reverse
    /// `uuid → gts_id` index is pruned in lock-step.
    pub fn retain(&self, predicate: &dyn Fn(&V) -> bool) {
        let mut guard = self.inner.lock();
        let to_remove: Vec<String> = guard
            .lru
            .iter()
            .filter(|(_, entry)| !predicate(&entry.value))
            .map(|(k, _)| k.clone())
            .collect();
        for key in to_remove {
            guard.pop_with_cleanup(&key);
        }
    }

    /// Clears every entry, including the reverse `uuid → gts_id` index.
    pub fn clear(&self) {
        let mut guard = self.inner.lock();
        guard.lru.clear();
        guard.uuid_to_id.clear();
    }

    /// Number of entries currently held (including not-yet-expired stale ones).
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().lru.len()
    }

    /// Returns `true` if the cache has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().lru.is_empty()
    }
}

impl<V> Cache<V> for InMemoryCache<V>
where
    V: Clone + HasUuid + Send + Sync + 'static,
{
    fn get(&self, key: &str) -> Option<V> {
        Self::get(self, key)
    }
    fn get_many(&self, keys: &[&str]) -> Vec<Option<V>> {
        Self::get_many(self, keys)
    }
    fn get_by_uuid(&self, uuid: Uuid) -> Option<V> {
        Self::get_by_uuid(self, uuid)
    }
    fn get_many_by_uuid(&self, uuids: &[Uuid]) -> Vec<Option<V>> {
        Self::get_many_by_uuid(self, uuids)
    }
    fn put(&self, key: String, value: V) {
        Self::put(self, key, value);
    }
    fn put_many(&self, entries: Vec<(String, V)>) {
        Self::put_many(self, entries);
    }
    fn invalidate(&self, key: &str) {
        Self::invalidate(self, key);
    }
    fn retain(&self, predicate: &dyn Fn(&V) -> bool) {
        Self::retain(self, predicate);
    }
    fn clear(&self) {
        Self::clear(self);
    }
    fn len(&self) -> usize {
        Self::len(self)
    }
    fn is_empty(&self) -> bool {
        Self::is_empty(self)
    }
}

/// Boxed [`Cache`] of resolved type-schemas (Arc-shared so ancestor chains
/// dedupe). The local client owns this as a trait object so the concrete
/// backend (in-memory today, Redis tomorrow) can be swapped without
/// touching consumer code.
pub type TypeSchemaCache = Box<dyn Cache<Arc<GtsTypeSchema>>>;

/// Boxed [`Cache`] of resolved instances. Same swap-friendly shape as
/// [`TypeSchemaCache`].
pub type InstanceCache = Box<dyn Cache<Arc<GtsInstance>>>;

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
