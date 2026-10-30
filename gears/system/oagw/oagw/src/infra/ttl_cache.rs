//! Bounded, sharded LRU cache with per-entry TTL.

use std::hash::{BuildHasher, Hash};
use std::num::NonZeroUsize;
use std::time::Duration;

use lru::LruCache;
use parking_lot::Mutex;
use tokio::time::Instant;

/// Shards spread lock contention; capacity is split evenly across them.
const SHARDS: usize = 16;

/// One shard: values with their optional expiry instant.
type Shard<K, V> = Mutex<LruCache<K, (V, Option<Instant>)>>;

/// Least-recently-used cache whose entries also expire after their TTL.
///
/// Memory is bounded by the capacity given at construction; the oldest
/// entries are evicted (dropped) first. Expired entries are dropped when
/// looked up.
pub(crate) struct TtlCache<K, V> {
    shards: Vec<Shard<K, V>>,
    hasher: std::collections::hash_map::RandomState,
}

impl<K: Hash + Eq, V: Clone> TtlCache<K, V> {
    /// A cache holding at most about `capacity` entries (rounded up to a
    /// multiple of the shard count). Zero disables caching.
    pub(crate) fn new(capacity: usize) -> Self {
        let per_shard = capacity.div_ceil(SHARDS);
        let shards = match NonZeroUsize::new(per_shard) {
            Some(cap) => (0..SHARDS)
                .map(|_| Mutex::new(LruCache::new(cap)))
                .collect(),
            None => Vec::new(),
        };
        Self {
            shards,
            hasher: std::collections::hash_map::RandomState::new(),
        }
    }

    fn shard(&self, key: &K) -> Option<&Shard<K, V>> {
        if self.shards.is_empty() {
            return None;
        }
        let idx = usize::try_from(self.hasher.hash_one(key) % SHARDS as u64).unwrap_or(0);
        self.shards.get(idx)
    }

    /// The live value for `key`, if any.
    pub(crate) fn get(&self, key: &K) -> Option<V> {
        let mut shard = self.shard(key)?.lock();
        match shard.get(key) {
            Some((value, expires)) if expires.is_none_or(|at| Instant::now() < at) => {
                Some(value.clone())
            }
            Some(_) => {
                shard.pop(key);
                None
            }
            None => None,
        }
    }

    /// Insert `value`. `ttl == None` never expires; a zero TTL is not
    /// cached at all.
    pub(crate) fn put(&self, key: K, value: V, ttl: Option<Duration>) {
        if ttl.is_some_and(|t| t.is_zero()) {
            return;
        }
        let Some(shard) = self.shard(&key) else {
            return;
        };
        // A TTL too large to represent never expires (instead of panicking).
        let expires = ttl.and_then(|t| Instant::now().checked_add(t));
        shard.lock().put(key, (value, expires));
    }

    /// Remove `key`.
    #[cfg(test)]
    pub(crate) fn remove(&self, key: &K) {
        if let Some(shard) = self.shard(key) {
            shard.lock().pop(key);
        }
    }

    /// Number of stored (possibly expired) entries.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn huge_ttl_never_expires_instead_of_panicking() {
        let c = TtlCache::new(10);
        c.put("a", 1, Some(Duration::MAX));
        assert_eq!(c.get(&"a"), Some(1));
    }

    #[tokio::test]
    async fn put_get_remove() {
        let c = TtlCache::new(10);
        c.put("a", 1, Some(Duration::from_secs(60)));
        assert_eq!(c.get(&"a"), Some(1));
        c.remove(&"a");
        assert_eq!(c.get(&"a"), None);
    }

    #[tokio::test]
    async fn zero_ttl_is_not_cached() {
        let c = TtlCache::new(10);
        c.put("a", 1, Some(Duration::ZERO));
        assert_eq!(c.get(&"a"), None);
        assert_eq!(c.len(), 0);
    }

    #[tokio::test]
    async fn no_ttl_never_expires() {
        let c = TtlCache::new(10);
        c.put("a", 1, None);
        assert_eq!(c.get(&"a"), Some(1));
    }

    #[tokio::test(start_paused = true)]
    async fn entries_expire() {
        let c = TtlCache::new(10);
        c.put("a", 1, Some(Duration::from_secs(5)));
        tokio::time::advance(Duration::from_secs(6)).await;
        assert_eq!(c.get(&"a"), None);
        assert_eq!(c.len(), 0, "expired entry dropped on lookup");
    }

    #[tokio::test]
    async fn capacity_is_bounded() {
        let c = TtlCache::new(32);
        for i in 0..10_000 {
            c.put(i, i, None);
        }
        assert!(c.len() <= 32);
    }

    #[tokio::test]
    async fn zero_capacity_disables() {
        let c = TtlCache::new(0);
        c.put("a", 1, None);
        assert_eq!(c.get(&"a"), None);
    }
}
