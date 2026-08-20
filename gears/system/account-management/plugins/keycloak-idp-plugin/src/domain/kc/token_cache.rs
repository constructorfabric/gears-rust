//! In-memory token cache keyed by `(realm_name, client_id)`.
//!
//! See DESIGN "Component Model" and the `CachedToken` shape table.
//! Token eviction is **read-time**: a fetched entry whose `expires_at` is in
//! the past is dropped from the map and the caller re-acquires. Physical
//! eviction is `remove_if`-gated on `Arc::ptr_eq` so a concurrent `insert` of
//! a fresh token cannot be clobbered by the late-arriving eviction.

use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use secrecy::SecretString;
use toolkit_macros::domain_model;

pub type RealmTokenEntries = DashMap<String, Arc<CachedToken>>;
pub type RealmInflightEntries = DashMap<String, Arc<tokio::sync::Mutex<()>>>;

/// In-memory cached admin token + its realm-relative base URL.
#[domain_model]
#[derive(Clone)]
pub struct CachedToken {
    /// Bearer value. Redacted from `Debug` via `secrecy::SecretString`.
    pub access_token: SecretString,
    /// `now() + expires_in - admin_token_lifetime_safety_ms` per DESIGN "Component Model".
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
    /// Two-level layout lets both realm and client id use `String`'s borrowed
    /// `str` lookup. Warm-cache reads therefore allocate no key material.
    pub inner: DashMap<String, RealmTokenEntries>,
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
        let entry_arc = {
            let realm_entries = self.inner.get(realm)?;
            let entry = realm_entries.get(client_id)?;
            Arc::clone(entry.value())
        };
        if Instant::now() >= entry_arc.expires_at {
            if let Some(realm_entries) = self.inner.get(realm) {
                realm_entries.remove_if(client_id, |_, v| Arc::ptr_eq(v, &entry_arc));
            }
            self.remove_realm_if_empty(realm);
            None
        } else {
            Some(entry_arc)
        }
    }

    /// Insert or replace the entry for `(realm, client_id)`.
    pub fn insert(&self, realm: &str, client_id: &str, token: Arc<CachedToken>) {
        if let Some(realm_entries) = self.inner.get(realm) {
            realm_entries.insert(client_id.to_owned(), token);
            return;
        }
        self.inner
            .entry(realm.to_owned())
            .or_default()
            .insert(client_id.to_owned(), token);
    }

    /// Drop the entry for `(realm, client_id)`. No-op if absent.
    pub fn invalidate(&self, realm: &str, client_id: &str) {
        if let Some(realm_entries) = self.inner.get(realm) {
            realm_entries.remove(client_id);
        }
        self.remove_realm_if_empty(realm);
    }

    /// Scan + remove every entry whose `expires_at` has passed. Returns
    /// the number of entries dropped (useful as a metrics gauge once
    /// the KC observability layer wires `keycloak_idp_plugin.kc_token_cache_evicted`).
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
        let mut removed = 0usize;
        self.inner.retain(|_, realm_entries| {
            realm_entries.retain(|_, token| {
                let keep = now < token.expires_at;
                removed += usize::from(!keep);
                keep
            });
            !realm_entries.is_empty()
        });
        removed
    }

    fn remove_realm_if_empty(&self, realm: &str) {
        self.inner
            .remove_if(realm, |_, realm_entries| realm_entries.is_empty());
    }
}

/// Per-key locks used by the factory to single-flight cold-cache token
/// fetches (DESIGN "Component Model" — single-flight on miss). The lock is held
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
    /// Mirrors [`TokenCache`]'s borrowed two-level lookup so existing lock
    /// entries can be found without allocating a tuple of owned strings.
    pub locks: DashMap<String, RealmInflightEntries>,
}

impl InflightLocks {
    /// Return the lock for `(realm, client_id)`, inserting an empty one if
    /// absent. The returned `Arc` lets the caller hold the lock without
    /// keeping the `DashMap` shard pinned.
    #[must_use]
    pub fn lock_for(&self, realm: &str, client_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        if let Some(realm_entries) = self.locks.get(realm) {
            return Self::lock_for_client(&realm_entries, client_id);
        }
        let realm_entries = self.locks.entry(realm.to_owned()).or_default();
        Self::lock_for_client(&realm_entries, client_id)
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
        if let Some(realm_entries) = self.locks.get(realm) {
            realm_entries.remove(client_id);
        }
        self.locks
            .remove_if(realm, |_, realm_entries| realm_entries.is_empty());
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
        let mut removed = 0usize;
        // `TokenCache::get` does read-time eviction, so its `is_some`
        // answer reflects the cache's POST-prune state. Calling it
        // here without a paired `prune_expired` is fine but less
        // efficient — the typical caller invokes both back-to-back.
        self.locks.retain(|realm, realm_entries| {
            realm_entries.retain(|client_id, _| {
                let keep = token_cache.get(realm, client_id).is_some();
                removed += usize::from(!keep);
                keep
            });
            !realm_entries.is_empty()
        });
        removed
    }

    fn lock_for_client(
        realm_entries: &RealmInflightEntries,
        client_id: &str,
    ) -> Arc<tokio::sync::Mutex<()>> {
        if let Some(lock) = realm_entries.get(client_id) {
            return Arc::clone(lock.value());
        }
        realm_entries
            .entry(client_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
}

#[cfg(test)]
#[path = "token_cache_tests.rs"]
mod tests;
