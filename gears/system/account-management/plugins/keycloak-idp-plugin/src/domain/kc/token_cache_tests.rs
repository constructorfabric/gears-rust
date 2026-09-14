use super::*;
use std::time::Duration;

// `len()` accessors live here rather than on the production structs:
// only tests need them today, and DE1101 forbids `#[cfg(test)] fn` items
// in a production file that has a companion test file. Promote back to
// `pub(crate)` on the parent impl when the metrics layer wires the
// `keycloak_idp_plugin.kc_token_cache_size` / `kc_inflight_locks` gauges in
// the follow-up KC HTTP refactor.
impl TokenCache {
    #[must_use]
    fn len(&self) -> usize {
        self.inner.iter().map(|realm| realm.value().len()).sum()
    }
}

impl InflightLocks {
    #[must_use]
    fn len(&self) -> usize {
        self.locks.iter().map(|realm| realm.value().len()).sum()
    }
}

fn token(ttl: Duration) -> Arc<CachedToken> {
    Arc::new(CachedToken {
        access_token: SecretString::from(String::from("dummy")),
        expires_at: Instant::now() + ttl,
        realm_url: Arc::from("https://kc/admin/realms/master"),
    })
}

/// Build an explicitly-expired token. Subtracting from `Instant::now()`
/// is the canonical way to land "expired" without sleeping — the
/// monotonic clock allows it on every supported platform.
fn expired_token() -> Arc<CachedToken> {
    Arc::new(CachedToken {
        access_token: SecretString::from(String::from("dummy")),
        expires_at: Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("monotonic clock allows subtracting 1s"),
        realm_url: Arc::from("https://kc"),
    })
}

#[test]
fn miss_when_empty() {
    let cache = TokenCache::default();
    assert!(cache.get("master", "x").is_none());
}

#[test]
fn hit_when_present_and_not_expired() {
    let cache = TokenCache::default();
    cache.insert("master", "x", token(Duration::from_mins(1)));
    assert!(cache.get("master", "x").is_some());
}

#[test]
fn miss_when_expired_and_physically_evicts() {
    let cache = TokenCache::default();
    cache.insert("master", "x", expired_token());
    assert_eq!(cache.len(), 1, "expired entry is present before the read");
    assert!(cache.get("master", "x").is_none());
    assert_eq!(
        cache.len(),
        0,
        "read-time eviction must physically drop the expired entry"
    );
}

#[test]
fn invalidate_removes_entry() {
    let cache = TokenCache::default();
    cache.insert("master", "x", token(Duration::from_mins(1)));
    assert_eq!(cache.len(), 1);
    cache.invalidate("master", "x");
    assert_eq!(cache.len(), 0);
    assert!(cache.get("master", "x").is_none());
    assert!(!cache.inner.contains_key("master"));
}

#[test]
fn different_realms_are_independent() {
    let cache = TokenCache::default();
    cache.insert("master", "x", token(Duration::from_mins(1)));
    cache.insert("platform", "x", token(Duration::from_mins(1)));
    assert_eq!(cache.len(), 2);
    cache.invalidate("master", "x");
    assert!(cache.get("master", "x").is_none());
    assert!(cache.get("platform", "x").is_some());
}

#[test]
fn debug_redacts_access_token() {
    let t = token(Duration::from_mins(1));
    let dbg = format!("{t:?}");
    assert!(
        !dbg.contains("dummy"),
        "Debug must redact token value: {dbg}"
    );
    assert!(dbg.contains("<redacted>"));
}

#[test]
fn inflight_forget_drops_entry() {
    let locks = InflightLocks::default();
    let _l1 = locks.lock_for("partner-a", "keycloak-idp-plugin-realm-admin");
    let _l2 = locks.lock_for("partner-b", "keycloak-idp-plugin-realm-admin");
    assert_eq!(locks.len(), 2);
    locks.forget("partner-a", "keycloak-idp-plugin-realm-admin");
    assert_eq!(locks.len(), 1);
    assert!(!locks.locks.contains_key("partner-a"));
    // Forgetting an absent entry is a no-op.
    locks.forget("ghost", "x");
    assert_eq!(locks.len(), 1);
}

#[test]
fn inflight_lock_for_inserts_then_returns_same_arc() {
    let locks = InflightLocks::default();
    let a = locks.lock_for("partner-a", "keycloak-idp-plugin-realm-admin");
    let b = locks.lock_for("partner-a", "keycloak-idp-plugin-realm-admin");
    assert!(Arc::ptr_eq(&a, &b), "same key must hand back the same Arc");
}

#[test]
fn prune_expired_drops_only_past_entries() {
    let cache = TokenCache::default();
    cache.insert("realm-a", "x", token(Duration::from_mins(1))); // live
    cache.insert("realm-b", "x", expired_token()); // stale
    cache.insert("realm-c", "x", token(Duration::from_mins(1))); // live
    cache.insert("realm-d", "x", expired_token()); // stale

    let evicted = cache.prune_expired();
    assert_eq!(evicted, 2);
    assert_eq!(cache.len(), 2);
    assert!(cache.get("realm-a", "x").is_some());
    assert!(cache.get("realm-b", "x").is_none());
    assert!(cache.get("realm-c", "x").is_some());
    assert!(cache.get("realm-d", "x").is_none());
}

#[test]
fn prune_expired_is_noop_when_cache_is_warm() {
    let cache = TokenCache::default();
    cache.insert("realm-a", "x", token(Duration::from_mins(1)));
    cache.insert("realm-b", "x", token(Duration::from_mins(1)));
    assert_eq!(cache.prune_expired(), 0);
    assert_eq!(cache.len(), 2);
}

#[test]
fn inflight_prune_inactive_drops_orphan_lock_entries() {
    let cache = TokenCache::default();
    let locks = InflightLocks::default();
    // Three realms have an inflight lock entry; only two have a
    // live token (realm-c was provisioned then never re-fetched,
    // its TokenCache entry expired and was already read-time-evicted).
    drop(locks.lock_for("realm-a", "x"));
    drop(locks.lock_for("realm-b", "x"));
    drop(locks.lock_for("realm-c", "x"));
    cache.insert("realm-a", "x", token(Duration::from_mins(1)));
    cache.insert("realm-b", "x", token(Duration::from_mins(1)));

    let dropped = locks.prune_inactive(&cache);
    assert_eq!(dropped, 1, "only realm-c (no live token) must be reclaimed");
    assert_eq!(locks.len(), 2);
}

#[test]
fn inflight_prune_inactive_also_drops_lock_for_freshly_expired_tokens() {
    let cache = TokenCache::default();
    let locks = InflightLocks::default();
    // realm-a has a live token, realm-b's token is expired (but
    // still physically present in the cache — eviction is
    // read-time).
    cache.insert("realm-a", "x", token(Duration::from_mins(1)));
    cache.insert("realm-b", "x", expired_token());
    drop(locks.lock_for("realm-a", "x"));
    drop(locks.lock_for("realm-b", "x"));

    // `prune_inactive` calls `TokenCache::get`, which triggers
    // read-time eviction for the expired entry and then sees no
    // live token → drops the lock. Net result: realm-b is
    // reclaimed from BOTH maps in one sweep.
    let dropped = locks.prune_inactive(&cache);
    assert_eq!(dropped, 1);
    assert_eq!(
        cache.len(),
        1,
        "expired token must be evicted by the get inside prune_inactive"
    );
}
