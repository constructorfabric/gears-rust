//! In-memory token cache keyed by `(realm_name, client_id)`.
//!
//! See DESIGN §4.4 "Shared implementation" and the `CachedToken` shape table.
//! Token eviction is **read-time**: a fetched entry whose `expires_at` is in
//! the past is dropped from the map and the caller re-acquires. Physical
//! eviction is `remove_if`-gated on `Arc::ptr_eq` so a concurrent `insert` of
//! a fresh token cannot be clobbered by the late-arriving eviction.

use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use secrecy::SecretString;
use toolkit_macros::domain_model;

/// In-memory cached admin token + its realm-relative base URL.
#[domain_model]
#[derive(Clone)]
pub struct CachedToken {
    /// Bearer value. Redacted from `Debug` via `secrecy::SecretString`.
    pub access_token: SecretString,
    /// `now() + expires_in - admin_token_lifetime_safety_ms` per DESIGN §4.4.
    pub expires_at: Instant,
    /// Pre-built `{base_url}/admin/realms/{realm}` for this token's scope.
    pub realm_url: Arc<str>,
}

impl std::fmt::Debug for CachedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedToken")
            .field("access_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("realm_url", &self.realm_url)
            .finish()
    }
}

/// Concurrent token cache. Backed by `DashMap` so reads don't block writers.
///
/// `inner` is `pub(crate)` so the cfg(test)-gated companion `token_cache_tests`
/// module can attach a `len()` accessor without splitting tests across two
/// files (DE1101). The field is otherwise an implementation detail of this
/// module.
#[domain_model]
#[derive(Default)]
pub struct TokenCache {
    pub(crate) inner: DashMap<(String, String), Arc<CachedToken>>,
}

impl TokenCache {
    /// Return the cached token for `(realm, client_id)` if present **and not yet expired**.
    /// Returns `None` on miss OR on expired entry (caller should re-acquire).
    ///
    /// An expired entry is physically removed from the map (read-time
    /// eviction), keeping the cache bounded under realm churn without an
    /// explicit `invalidate`. The removal is guarded by `Arc::ptr_eq` so a
    /// concurrent `insert` of a fresh token wins the race.
    #[must_use]
    pub fn get(&self, realm: &str, client_id: &str) -> Option<Arc<CachedToken>> {
        let key = (realm.to_owned(), client_id.to_owned());
        let entry_arc = {
            let entry = self.inner.get(&key)?;
            Arc::clone(entry.value())
        };
        if Instant::now() >= entry_arc.expires_at {
            self.inner
                .remove_if(&key, |_, v| Arc::ptr_eq(v, &entry_arc));
            None
        } else {
            Some(entry_arc)
        }
    }

    /// Insert or replace the entry for `(realm, client_id)`.
    pub fn insert(&self, realm: &str, client_id: &str, token: Arc<CachedToken>) {
        self.inner
            .insert((realm.to_owned(), client_id.to_owned()), token);
    }

    /// Drop the entry for `(realm, client_id)`. No-op if absent.
    pub fn invalidate(&self, realm: &str, client_id: &str) {
        self.inner.remove(&(realm.to_owned(), client_id.to_owned()));
    }

    /// Scan + remove every entry whose `expires_at` has passed. Returns
    /// the number of entries dropped (useful as a metrics gauge once
    /// the KC observability layer wires `vp_idp_plugin.kc_token_cache_evicted`).
    ///
    /// Read-time eviction in [`Self::get`] already keeps the cache
    /// bounded by *actively-fetched* realms; this method bounds it by
    /// *active-token* realms — a realm that was fetched once, expired,
    /// and never re-accessed has its entry physically removed without
    /// waiting for a re-read. Called from the factory's slow-path
    /// `acquire_client` to amortise the sweep across cold-cache hits.
    #[must_use = "the eviction count is a useful gauge; if you don't need it, prefix with `_ =`"]
    pub fn prune_expired(&self) -> usize {
        let now = Instant::now();
        let before = self.inner.len();
        self.inner.retain(|_, v| now < v.expires_at);
        before - self.inner.len()
    }
}

/// Per-key locks used by the factory to single-flight cold-cache token
/// fetches (DESIGN §4.4 line 197 — single-flight on miss). The lock is held
/// only across the secret-resolve + token-fetch path; warm-cache reads do
/// NOT touch the lock map.
///
/// The map grows O(realms × `admin_client_ids`) and is reclaimed by
/// [`InflightLocks::forget`] (called from the factory's `invalidate`
/// path) and [`InflightLocks::prune_inactive`] (paired with
/// [`TokenCache::prune_expired`] on the slow-path sweep).
///
/// `locks` is `pub(crate)` for the same reason as [`TokenCache::inner`] —
/// the cfg(test) companion attaches a `len()` accessor without falling
/// foul of DE1101's "tests-not-split" rule.
#[domain_model]
#[derive(Default)]
pub struct InflightLocks {
    pub(crate) locks: DashMap<(String, String), Arc<tokio::sync::Mutex<()>>>,
}

impl InflightLocks {
    /// Return the lock for `(realm, client_id)`, inserting an empty one if
    /// absent. The returned `Arc` lets the caller hold the lock without
    /// keeping the `DashMap` shard pinned.
    #[must_use]
    pub fn lock_for(&self, realm: &str, client_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .entry((realm.to_owned(), client_id.to_owned()))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Drop the lock entry for `(realm, client_id)`. Called by the
    /// factory's `invalidate` path so deprovisioned realms don't leak
    /// `Arc<Mutex<_>>` entries forever. Safe to call concurrently with
    /// in-flight `lock_for` waiters — `DashMap::remove` is atomic, and
    /// any waiter still holding its `Arc` keeps the mutex alive until
    /// it drops the guard. A subsequent `lock_for` allocates a fresh
    /// entry, which is fine because the slow-path single-flight
    /// invariant only matters per-call (re-check the cache after lock
    /// acquisition, see `acquire_client`).
    pub fn forget(&self, realm: &str, client_id: &str) {
        self.locks.remove(&(realm.to_owned(), client_id.to_owned()));
    }

    /// Drop every lock entry whose `(realm, client_id)` no longer has
    /// a live entry in `token_cache`. Pairs with
    /// [`TokenCache::prune_expired`]: after a sweep, idle realms get
    /// their inflight lock entry reclaimed too. A caller concurrently
    /// holding an `Arc` to the lock keeps it alive until its guard
    /// drops — only the map slot is reclaimed, so single-flight stays
    /// honoured for in-progress acquires. Returns the number of
    /// entries dropped (gauge candidate).
    #[must_use = "the eviction count is a useful gauge; if you don't need it, prefix with `_ =`"]
    pub fn prune_inactive(&self, token_cache: &TokenCache) -> usize {
        let before = self.locks.len();
        // `TokenCache::get` does read-time eviction, so its `is_some`
        // answer reflects the cache's POST-prune state. Calling it
        // here without a paired `prune_expired` is fine but less
        // efficient — the typical caller invokes both back-to-back.
        self.locks
            .retain(|key, _| token_cache.get(&key.0, &key.1).is_some());
        before - self.locks.len()
    }
}

#[cfg(test)]
#[path = "token_cache_tests.rs"]
mod tests;
