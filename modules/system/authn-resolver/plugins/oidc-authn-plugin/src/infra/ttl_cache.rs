//! Bounded TTL cache backed by [`DashMap`].
//!
//! Provides time-based expiry and least-recently-used eviction when at capacity.
//! Used by both the JWKS cache and OIDC Discovery cache to avoid duplicating
//! the insert-with-eviction logic.

use std::time::{Duration, Instant};

use dashmap::DashMap;

/// Cache entries must expose their fetch timestamp for TTL and eviction ordering.
pub trait Timestamped {
    /// When this entry was fetched from the upstream source.
    fn fetched_at(&self) -> Instant;

    /// Optional per-entry TTL override. When `Some`, `get_fresh` uses this
    /// instead of the cache-level TTL — allows entries whose upstream
    /// `expires_in` is shorter than the global cache TTL to expire earlier.
    fn effective_ttl(&self) -> Option<Duration> {
        None
    }
}

#[derive(Debug, Clone)]
struct CacheSlot<V> {
    value: V,
    last_accessed: Instant,
}

/// Bounded in-memory cache with TTL expiry and least-recently-used eviction.
///
/// Thread-safe via [`DashMap`]. Entries are keyed by `String` (typically an
/// issuer URL). When inserting a **new** key at capacity, the least-recently-used
/// entry is evicted. Re-inserting an existing key (refresh)
/// does not trigger eviction.
pub struct TtlCache<V> {
    inner: DashMap<String, CacheSlot<V>>,
    ttl: Duration,
    max_entries: usize,
}

impl<V> std::fmt::Debug for TtlCache<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TtlCache")
            .field("entries", &self.inner.len())
            .field("ttl", &self.ttl)
            .field("max_entries", &self.max_entries)
            .finish()
    }
}

impl<V: Timestamped + Clone> TtlCache<V> {
    /// Create a new cache with the given TTL and maximum entry count.
    ///
    /// `max_entries` is clamped to at least 1.
    pub(crate) fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            inner: DashMap::new(),
            ttl,
            max_entries: max_entries.max(1),
        }
    }

    /// Return a fresh (non-expired) entry, or `None` if absent or stale.
    ///
    /// Uses the entry's per-entry TTL override when present, falling back to
    /// the cache-level TTL configured at construction.
    pub(crate) fn get_fresh(&self, key: &str) -> Option<V> {
        self.inner.get_mut(key).and_then(|mut slot| {
            let ttl = slot.value().value.effective_ttl().unwrap_or(self.ttl);

            if slot.value().value.fetched_at().elapsed() <= ttl {
                slot.value_mut().last_accessed = Instant::now();

                Some(slot.value().value.clone())
            } else {
                None
            }
        })
    }

    /// Return a cached entry only if its fetched age is no greater than `max_age`.
    pub(crate) fn get_if_age_at_most(&self, key: &str, max_age: Duration) -> Option<V> {
        self.inner.get_mut(key).and_then(|mut slot| {
            if slot.value().value.fetched_at().elapsed() <= max_age {
                slot.value_mut().last_accessed = Instant::now();

                Some(slot.value().value.clone())
            } else {
                None
            }
        })
    }

    /// Insert an entry, evicting the oldest if a **new** key is added at capacity.
    ///
    /// `cache_label` is used in the eviction warning log (e.g. `"JWKS"` or
    /// `"OIDC discovery"`).
    ///
    /// **Concurrency note:** The `contains_key` check and subsequent `insert`
    /// are not atomic across `DashMap` shards. Under concurrent access two threads
    /// could both pass the capacity guard and both insert, transiently pushing
    /// the cache to `max_entries + N` items. This is acceptable for a cache:
    /// the overshoot is bounded by the number of concurrent callers and
    /// self-corrects on the next eviction-eligible insert.
    pub(crate) fn insert_with_eviction(&self, key: &str, value: V, cache_label: &str) {
        if self.inner.len() >= self.max_entries
            && !self.inner.contains_key(key)
            && let Some(oldest_key) = self
                .inner
                .iter()
                .min_by_key(|entry| entry.value().last_accessed)
                .map(|entry| entry.key().clone())
        {
            tracing::warn!(
                max_entries = self.max_entries,
                evicted_issuer = %oldest_key,
                "{cache_label} cache at capacity, evicting least-recently-used entry"
            );

            self.inner.remove(&oldest_key);
        }

        self.inner.insert(
            key.to_owned(),
            CacheSlot {
                value,
                last_accessed: Instant::now(),
            },
        );
    }

    /// Insert directly without eviction (for unit test injection and similar).
    #[cfg(test)]
    pub(crate) fn insert(&self, key: &str, value: V) {
        self.inner.insert(
            key.to_owned(),
            CacheSlot {
                value,
                last_accessed: Instant::now(),
            },
        );
    }

    /// Insert directly with an explicit last-accessed timestamp (for tests).
    #[cfg(test)]
    pub(crate) fn insert_with_last_accessed(&self, key: &str, value: V, last_accessed: Instant) {
        self.inner.insert(
            key.to_owned(),
            CacheSlot {
                value,
                last_accessed,
            },
        );
    }

    /// Whether the cache contains an entry for the given key (possibly stale).
    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.inner.contains_key(key)
    }

    /// Current number of entries in the cache.
    pub(crate) fn len(&self) -> usize {
        self.inner.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct Entry {
        ts: Instant,
    }

    impl Entry {
        fn new() -> Self {
            Self { ts: Instant::now() }
        }

        fn at(ts: Instant) -> Self {
            Self { ts }
        }
    }

    impl Timestamped for Entry {
        fn fetched_at(&self) -> Instant {
            self.ts
        }
    }

    #[test]
    fn get_fresh_returns_non_expired_entry() {
        let cache = TtlCache::new(Duration::from_hours(1), 10);
        cache.insert("k", Entry::new());
        assert!(cache.get_fresh("k").is_some());
    }

    #[test]
    fn get_fresh_returns_none_for_expired_entry() {
        let cache = TtlCache::new(Duration::from_secs(0), 10);
        cache.insert("k", Entry::new());
        assert!(cache.get_fresh("k").is_none());
    }

    #[test]
    fn eviction_removes_least_recently_used_on_new_key_at_capacity() {
        let now = Instant::now();
        let older = now.checked_sub(Duration::from_secs(100)).unwrap_or(now);
        let cache: TtlCache<Entry> = TtlCache::new(Duration::from_hours(1), 2);
        cache.insert_with_last_accessed("old", Entry::at(now), older);
        cache.insert_with_last_accessed("new", Entry::at(now), now);

        cache.insert_with_eviction("third", Entry::new(), "test");

        assert!(
            !cache.contains_key("old"),
            "least recently used entry should be evicted"
        );
        assert!(cache.contains_key("new"));
        assert!(cache.contains_key("third"));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn cache_hit_updates_lru_position() {
        let now = Instant::now();
        let older = now.checked_sub(Duration::from_secs(100)).unwrap_or(now);
        let cache: TtlCache<Entry> = TtlCache::new(Duration::from_hours(1), 2);
        cache.insert_with_last_accessed("a", Entry::at(now), older);
        cache.insert_with_last_accessed("b", Entry::at(now), now);

        assert!(cache.get_fresh("a").is_some());
        cache.insert_with_eviction("c", Entry::new(), "test");

        assert!(
            cache.contains_key("a"),
            "recently accessed entry should remain"
        );
        assert!(
            !cache.contains_key("b"),
            "least recently used entry should be evicted"
        );
        assert!(cache.contains_key("c"));
    }

    #[test]
    fn get_if_age_at_most_rejects_expired_stale_entry() {
        let cache: TtlCache<Entry> = TtlCache::new(Duration::from_secs(0), 10);
        let old = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        cache.insert("k", Entry::at(old));

        assert!(
            cache
                .get_if_age_at_most("k", Duration::from_secs(5))
                .is_none()
        );
        assert!(
            cache
                .get_if_age_at_most("k", Duration::from_secs(15))
                .is_some()
        );
    }

    #[test]
    fn refresh_existing_key_at_capacity_does_not_evict() {
        let now = Instant::now();
        let older = now.checked_sub(Duration::from_secs(100)).unwrap_or(now);
        let cache: TtlCache<Entry> = TtlCache::new(Duration::from_hours(1), 2);
        cache.insert("a", Entry::at(older));
        cache.insert("b", Entry::at(now));

        cache.insert_with_eviction("a", Entry::new(), "test");

        assert!(cache.contains_key("a"));
        assert!(cache.contains_key("b"));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn max_entries_clamped_to_one() {
        let cache: TtlCache<Entry> = TtlCache::new(Duration::from_hours(1), 0);
        cache.insert("a", Entry::new());
        assert_eq!(cache.len(), 1);
    }
}
