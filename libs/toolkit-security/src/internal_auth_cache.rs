//! Short-lived positive (and brief negative) caching for platform-plane
//! authentication.
//!
//! [`CachingInternalAuthenticator`] wraps any [`InternalAuthenticator`] with
//! an in-memory, TTL-bounded cache. It exists because a remote validation
//! backend (e.g. the Kubernetes `TokenReview` API) performs a live round-trip
//! on every call — untenable on a hot gRPC/HTTP path where the same projected
//! credential is presented on back-to-back requests
//! (`cpt-cf-adr-platform-plane-auth`, decision 5).
//!
//! # Semantics
//!
//! - **Successful** validations are cached for up to `ttl`, clamped to the
//!   credential's own remaining validity when it is a JWT carrying an `exp`
//!   claim (see [`jwt_exp_claim`]) — a token with two seconds left is never
//!   cached for the full configured `ttl`.
//! - **Rejections** ([`InternalAuthNError::InvalidToken`]) are cached for a
//!   short, fixed [`NEGATIVE_CACHE_TTL`] so a caller presenting no valid
//!   credential cannot drive one backend round-trip per request, while a
//!   token that becomes valid moments later is re-checked quickly.
//! - Backend failures ([`InternalAuthNError::Unavailable`], `Other`) are
//!   **never** cached: a transient outage is re-evaluated on the next call.
//! - Concurrent misses for the **same** token are serialized behind a
//!   per-token lock (single-flight), so a burst of calls carrying the same
//!   credential collapses into one backend round-trip instead of N.
//! - The cache key is the token itself, matched exactly. Exact matching
//!   avoids the collision risk of a non-cryptographic hash (two distinct
//!   tokens resolving to one cached identity) without pulling in a
//!   cryptographic digest — the workspace's validated crypto provider is
//!   platform-dependent, so this crate stays free of any direct crypto
//!   dependency. The token is already resident in process memory (request
//!   headers, the outbound credential), and entries are in-process and
//!   TTL-bounded.
//! - The cache holds at most [`MAX_CACHE_ENTRIES`] distinct tokens. When full,
//!   a single sweep reclaims any expired entries first (amortizing across the
//!   inserts it makes room for); only if nothing is reclaimable — a burst of
//!   distinct, individually-valid credentials — does it evict the entry
//!   expiring soonest, rather than growing unbounded.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;

use crate::internal_auth::{InternalAuthNError, InternalAuthenticator, PlatformIdentity};

/// Default time-to-live for a cached successful validation.
///
/// A conservative few seconds: long enough to collapse a burst of calls
/// carrying the same token, short enough to keep the post-revocation
/// acceptance window small.
pub const DEFAULT_TOKEN_REVIEW_CACHE_TTL: Duration = Duration::from_secs(30);

/// Upper bound accepted by [`CachingInternalAuthenticator::new`]. Caps how
/// long a revoked or expired token can keep validating from cache, so a
/// misconfiguration cannot widen the revocation window unboundedly.
pub const MAX_TOKEN_REVIEW_CACHE_TTL: Duration = Duration::from_mins(5);

/// Fixed TTL for a cached **rejection**. Deliberately short and
/// non-configurable: it exists only to blunt a hot loop of fresh invalid
/// tokens, not to widen any acceptance window.
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(1);

/// Amortizes the expired-entry sweep: a full scan of the cache runs only
/// every `SWEEP_INTERVAL`-th insert rather than on every single one.
const SWEEP_INTERVAL: u32 = 32;

/// Maximum number of distinct tokens held at once, bounding memory even
/// under a sustained burst of distinct, individually-valid credentials
/// (which TTL expiry alone never reclaims). Generous enough for realistic
/// fleets of Kubernetes `ServiceAccount`s calling through a single
/// authenticator instance.
pub const MAX_CACHE_ENTRIES: usize = 10_000;

/// `ttl` passed to [`CachingInternalAuthenticator::new`] was zero or exceeded
/// [`MAX_TOKEN_REVIEW_CACHE_TTL`].
#[derive(Debug, thiserror::Error)]
#[error(
    "internal-auth cache TTL must be > 0 and <= {MAX_TOKEN_REVIEW_CACHE_TTL:?}, got {actual:?}"
)]
pub struct InvalidCacheTtl {
    actual: Duration,
}

/// A cached validation outcome and the instant it stops applying.
enum CacheEntry {
    Valid {
        identity: PlatformIdentity,
        expires_at: Instant,
    },
    Rejected {
        expires_at: Instant,
    },
}

impl CacheEntry {
    fn expires_at(&self) -> Instant {
        match self {
            Self::Valid { expires_at, .. } | Self::Rejected { expires_at } => *expires_at,
        }
    }
}

/// Outcome of a cache lookup.
enum CacheLookup {
    Valid(PlatformIdentity),
    Rejected,
    Miss,
}

/// Best-effort extraction of the `exp` (seconds since the Unix epoch) claim
/// from a JWT, without verifying the signature.
///
/// The caller has already had the token's signature verified by the
/// authentication backend (e.g. Kubernetes `TokenReview`); this is a plain
/// base64 decode of the already-trusted payload, used only to avoid caching a
/// validation past the credential's own expiry. Returns `None` for a
/// non-JWT credential (e.g. a shared secret), which simply skips the clamp.
fn jwt_exp_claim(token: &str) -> Option<u64> {
    let payload_b64 = token.split('.').nth(1)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    value.get("exp")?.as_u64()
}

/// The instant a freshly validated `token` should stop being trusted from
/// cache: whichever is sooner of `now + ttl` and the token's own `exp` claim
/// (when present).
fn clamped_expiry(token: &str, now: Instant, ttl: Duration) -> Instant {
    let ttl_expiry = now + ttl;
    let Some(exp_secs) = jwt_exp_claim(token) else {
        return ttl_expiry;
    };
    let exp_at = UNIX_EPOCH + Duration::from_secs(exp_secs);
    let Ok(remaining) = exp_at.duration_since(SystemTime::now()) else {
        // The token's own claim says it is already expired; do not extend
        // trust in it at all.
        return now;
    };
    ttl_expiry.min(now + remaining)
}

/// Wraps an [`InternalAuthenticator`] with a short-lived cache of both
/// successful and rejected validations.
///
/// Construct it around the concrete validator and hand the wrapper to the
/// transport layer as the `InternalAuthenticator`:
///
/// ```rust
/// use std::time::Duration;
/// use toolkit_security::{CachingInternalAuthenticator, InternalAuthNError, PlatformIdentity};
///
/// struct AlwaysOk;
/// impl toolkit_security::InternalAuthenticator for AlwaysOk {
///     async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
///         Ok(PlatformIdentity::Shared { name: token.to_owned() })
///     }
/// }
///
/// # fn wire() -> Result<(), Box<dyn std::error::Error>> {
/// let cached = CachingInternalAuthenticator::new(AlwaysOk, Duration::from_secs(30))?;
/// # let _ = cached;
/// # Ok(())
/// # }
/// ```
pub struct CachingInternalAuthenticator<A> {
    inner: A,
    ttl: Duration,
    cache: Mutex<HashMap<String, CacheEntry>>,
    /// Per-token single-flight locks: concurrent misses for the same token
    /// serialize here instead of each issuing a backend call.
    inflight: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    sweep_counter: AtomicU32,
}

impl<A> std::fmt::Debug for CachingInternalAuthenticator<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachingInternalAuthenticator")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl<A> CachingInternalAuthenticator<A> {
    /// Wrap `inner`, caching successful validations for up to `ttl` (clamped
    /// to the credential's own expiry when it is a JWT) and rejections for a
    /// short fixed window.
    ///
    /// # Errors
    /// Returns [`InvalidCacheTtl`] if `ttl` is zero or exceeds
    /// [`MAX_TOKEN_REVIEW_CACHE_TTL`].
    pub fn new(inner: A, ttl: Duration) -> Result<Self, InvalidCacheTtl> {
        if ttl == Duration::ZERO || ttl > MAX_TOKEN_REVIEW_CACHE_TTL {
            return Err(InvalidCacheTtl { actual: ttl });
        }
        Ok(Self {
            inner,
            ttl,
            cache: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
            sweep_counter: AtomicU32::new(0),
        })
    }

    /// Wrap `inner` with the [`DEFAULT_TOKEN_REVIEW_CACHE_TTL`].
    #[must_use]
    pub fn with_default_ttl(inner: A) -> Self {
        Self {
            inner,
            ttl: DEFAULT_TOKEN_REVIEW_CACHE_TTL,
            cache: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
            sweep_counter: AtomicU32::new(0),
        }
    }

    /// Look up a still-applicable cached outcome for `token`, evicting it
    /// immediately if found stale (rather than waiting for the next sweep).
    ///
    /// Holds the lock only for the duration of the map access (never across
    /// an `await`), so the returned future stays `Send`.
    fn lookup(&self, token: &str, now: Instant) -> CacheLookup {
        let mut cache = self.cache.lock();
        let Some(entry) = cache.get(token) else {
            return CacheLookup::Miss;
        };
        if entry.expires_at() <= now {
            cache.remove(token);
            return CacheLookup::Miss;
        }
        match entry {
            CacheEntry::Valid { identity, .. } => CacheLookup::Valid(identity.clone()),
            CacheEntry::Rejected { .. } => CacheLookup::Rejected,
        }
    }

    /// Insert `entry` under `token`, amortizing the expired-entry sweep over
    /// [`SWEEP_INTERVAL`] inserts instead of scanning the whole map every
    /// time, and enforcing [`MAX_CACHE_ENTRIES`] when a new token would
    /// exceed it.
    ///
    /// When full, expired entries are reclaimed with a single `retain` sweep
    /// first — one scan typically frees room for many subsequent inserts, so
    /// the following misses take the cheap path. Only if that sweep frees
    /// nothing (a fleet of simultaneously-valid tokens) does it fall back to
    /// the O(n) soonest-to-expire scan, so the whole-map scan is no longer
    /// paid on every miss while the cache stays full.
    fn insert(&self, token: String, entry: CacheEntry, now: Instant) {
        let mut cache = self.cache.lock();
        let count = self.sweep_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let swept = count.is_multiple_of(SWEEP_INTERVAL);
        if swept {
            cache.retain(|_, e| e.expires_at() > now);
        }
        if cache.len() >= MAX_CACHE_ENTRIES && !cache.contains_key(&token) {
            // Reclaim expired entries before scanning for a victim: a single
            // sweep amortizes across the many inserts it makes room for,
            // whereas the min_by_key scan below would otherwise run on every
            // miss for as long as the cache stayed full.
            if !swept {
                cache.retain(|_, e| e.expires_at() > now);
            }
            if cache.len() >= MAX_CACHE_ENTRIES {
                // Sweep freed nothing (every entry still valid): fall back to
                // evicting the soonest-to-expire, which needs re-validation
                // soonest regardless. No extra bookkeeping for real LRU order.
                if let Some(victim) = cache
                    .iter()
                    .min_by_key(|(_, e)| e.expires_at())
                    .map(|(k, _)| k.clone())
                {
                    cache.remove(&victim);
                }
            }
        }
        cache.insert(token, entry);
    }

    /// Get-or-create the per-token single-flight lock.
    fn token_lock(&self, token: &str) -> Arc<AsyncMutex<()>> {
        let mut inflight = self.inflight.lock();
        Arc::clone(
            inflight
                .entry(token.to_owned())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    }

    /// Drop the per-token lock from the map once nothing else references it,
    /// so `inflight` does not grow unboundedly over the process lifetime.
    fn release_token_lock(&self, token: &str, lock: &Arc<AsyncMutex<()>>) {
        let mut inflight = self.inflight.lock();
        // 2 = the map's own clone + `lock` here; anything higher means
        // another waiter still holds a clone.
        if Arc::strong_count(lock) <= 2 {
            inflight.remove(token);
        }
    }
}

/// Releases the `inflight` entry on drop, so cancellation (not just a normal
/// return) still cleans it up. Borrows `lock` rather than cloning it, so it
/// doesn't skew `release_token_lock`'s `Arc::strong_count` check.
struct ReleaseTokenLockOnDrop<'a, A> {
    owner: &'a CachingInternalAuthenticator<A>,
    token: &'a str,
    lock: &'a Arc<AsyncMutex<()>>,
}

impl<A> Drop for ReleaseTokenLockOnDrop<'_, A> {
    fn drop(&mut self) {
        self.owner.release_token_lock(self.token, self.lock);
    }
}

impl<A: InternalAuthenticator> CachingInternalAuthenticator<A> {
    /// The implementation behind [`InternalAuthenticator::authenticate`],
    /// parameterized on the "current" instant used for the lookup / miss
    /// phases.
    ///
    /// Split out so a test can drive a deterministic TTL-expiry check (e.g.
    /// `now + ttl + 1ms`) instead of a real `tokio::time::sleep` — this
    /// crate's `Instant`-based cache cannot be virtualized by
    /// `tokio::time::pause`. `authenticate` is simply the
    /// `Instant::now()`-sampling production wrapper.
    ///
    /// The instant used to timestamp a freshly-stored entry is still
    /// re-sampled internally *after* the backend round-trip (never derived
    /// from `now`) so a slow backend call never erodes the effective TTL.
    ///
    /// cancel-safe: dropping this future early loses only a would-be cache
    /// insert or single-flight slot — the lock and its `inflight` entry are
    /// still released, via `AsyncMutex`'s guard and [`ReleaseTokenLockOnDrop`].
    async fn authenticate_at(
        &self,
        token: &str,
        now: Instant,
    ) -> Result<PlatformIdentity, InternalAuthNError> {
        match self.lookup(token, now) {
            CacheLookup::Valid(identity) => return Ok(identity),
            CacheLookup::Rejected => return Err(InternalAuthNError::InvalidToken),
            CacheLookup::Miss => {}
        }

        // Single-flight: serialize concurrent misses for the same token so a
        // burst of calls collapses into one backend round-trip.
        let lock = self.token_lock(token);
        let _guard = lock.lock().await;
        // Cleans up `inflight` on drop too, so cancellation doesn't leak it.
        let _release = ReleaseTokenLockOnDrop {
            owner: self,
            token,
            lock: &lock,
        };

        // Another caller may have populated the cache while this one waited.
        // Refresh the cutoff: `now` predates the lock wait, so a stale value
        // could serve an entry that expired during it. Later of the injected
        // instant and real time keeps a test's deterministic `now` dominant.
        let now = now.max(Instant::now());
        match self.lookup(token, now) {
            CacheLookup::Valid(identity) => return Ok(identity),
            CacheLookup::Rejected => return Err(InternalAuthNError::InvalidToken),
            CacheLookup::Miss => {}
        }

        let result = self.inner.authenticate(token).await;
        // Re-sampled *after* the backend round-trip: sampling before it would
        // shrink the effective TTL by however long the call took.
        let stored_at = Instant::now();

        match result {
            Ok(identity) => {
                let expires_at = clamped_expiry(token, stored_at, self.ttl);
                self.insert(
                    token.to_owned(),
                    CacheEntry::Valid {
                        identity: identity.clone(),
                        expires_at,
                    },
                    stored_at,
                );
                Ok(identity)
            }
            Err(InternalAuthNError::InvalidToken) => {
                self.insert(
                    token.to_owned(),
                    CacheEntry::Rejected {
                        expires_at: stored_at + NEGATIVE_CACHE_TTL,
                    },
                    stored_at,
                );
                Err(InternalAuthNError::InvalidToken)
            }
            // Backend outage / unexpected failure: never cached, so a
            // recovery or a later attempt is re-evaluated immediately.
            Err(err) => Err(err),
        }
    }
}

impl<A: InternalAuthenticator> InternalAuthenticator for CachingInternalAuthenticator<A> {
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        self.authenticate_at(token, Instant::now()).await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Counts backend calls and can be flipped between success and one of two
    /// failure modes so tests can assert exactly when the wrapped
    /// authenticator is consulted.
    struct CountingAuth {
        calls: AtomicUsize,
        mode: Mutex<Mode>,
        /// Artificial delay before returning, to widen the single-flight
        /// window so a concurrent waiter genuinely blocks on the per-token
        /// lock instead of finding the cache already populated at its first
        /// (pre-lock) check.
        delay: Mutex<Duration>,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Succeed,
        Unavailable,
        Invalid,
    }

    impl CountingAuth {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                mode: Mutex::new(Mode::Succeed),
                delay: Mutex::new(Duration::ZERO),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
        fn set_mode(&self, mode: Mode) {
            *self.mode.lock() = mode;
        }
        fn set_delay(&self, delay: Duration) {
            *self.delay.lock() = delay;
        }
    }

    impl InternalAuthenticator for CountingAuth {
        async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let delay = *self.delay.lock();
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            match *self.mode.lock() {
                Mode::Succeed => Ok(PlatformIdentity::Shared {
                    name: token.to_owned(),
                }),
                Mode::Unavailable => Err(InternalAuthNError::Unavailable),
                Mode::Invalid => Err(InternalAuthNError::InvalidToken),
            }
        }
    }

    #[tokio::test]
    async fn second_call_within_ttl_hits_cache() {
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();

        let a = cached.authenticate("tok").await.unwrap();
        let b = cached.authenticate("tok").await.unwrap();
        assert_eq!(a, b);
        assert_eq!(
            cached.inner.calls(),
            1,
            "second call must be served from cache"
        );
        assert_eq!(
            a.peer_name(),
            "tok",
            "cached identity must match the token it was issued for"
        );
    }

    #[tokio::test]
    async fn distinct_tokens_are_cached_independently() {
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();

        let a = cached.authenticate("a").await.unwrap();
        let b = cached.authenticate("b").await.unwrap();
        let a2 = cached.authenticate("a").await.unwrap();
        assert_eq!(
            cached.inner.calls(),
            2,
            "each distinct token validated once"
        );
        assert_eq!(a.peer_name(), "a");
        assert_eq!(b.peer_name(), "b");
        assert_eq!(
            a2.peer_name(),
            "a",
            "cache must not confuse token identities"
        );
    }

    #[tokio::test]
    async fn entry_expires_after_ttl() {
        // Deterministic via `authenticate_at`, not a real sleep: this crate's
        // cache samples `Instant::now()` internally, which `tokio::time::pause`
        // cannot virtualize, so a real-time sleep would be both wall-clock
        // dependent and slow.
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_millis(20))
                .unwrap();
        let t0 = Instant::now();

        cached.authenticate_at("tok", t0).await.unwrap();
        assert_eq!(cached.inner.calls(), 1);

        cached
            .authenticate_at("tok", t0 + Duration::from_millis(21))
            .await
            .unwrap();
        assert_eq!(
            cached.inner.calls(),
            2,
            "expired entry must be re-validated"
        );
    }

    #[tokio::test]
    async fn unavailable_errors_are_not_cached() {
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();
        cached.inner.set_mode(Mode::Unavailable);

        assert!(cached.authenticate("tok").await.is_err());
        assert!(cached.authenticate("tok").await.is_err());
        assert_eq!(
            cached.inner.calls(),
            2,
            "backend outages must not be cached"
        );

        // Once the backend recovers, the next call succeeds and is then cached.
        cached.inner.set_mode(Mode::Succeed);
        cached.authenticate("tok").await.unwrap();
        cached.authenticate("tok").await.unwrap();
        assert_eq!(
            cached.inner.calls(),
            3,
            "recovery validated once, then cached"
        );
    }

    #[tokio::test]
    async fn invalid_token_rejections_are_briefly_negative_cached() {
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();
        cached.inner.set_mode(Mode::Invalid);

        let err = cached.authenticate("bad").await.unwrap_err();
        assert!(matches!(err, InternalAuthNError::InvalidToken));
        // A second rejection within the negative-cache window is served
        // without a second backend call.
        let err = cached.authenticate("bad").await.unwrap_err();
        assert!(matches!(err, InternalAuthNError::InvalidToken));
        assert_eq!(
            cached.inner.calls(),
            1,
            "a cached rejection must not re-hit the backend"
        );

        tokio::time::sleep(NEGATIVE_CACHE_TTL + Duration::from_millis(50)).await;
        cached.inner.set_mode(Mode::Succeed);
        let identity = cached.authenticate("bad").await.unwrap();
        assert_eq!(
            identity.peer_name(),
            "bad",
            "the token validates once the backend accepts it"
        );
        assert_eq!(cached.inner.calls(), 2);
    }

    #[test]
    fn new_rejects_zero_and_over_max_ttl() {
        assert!(CachingInternalAuthenticator::new(CountingAuth::new(), Duration::ZERO).is_err());
        assert!(
            CachingInternalAuthenticator::new(
                CountingAuth::new(),
                MAX_TOKEN_REVIEW_CACHE_TTL + Duration::from_secs(1)
            )
            .is_err()
        );
        assert!(
            CachingInternalAuthenticator::new(CountingAuth::new(), MAX_TOKEN_REVIEW_CACHE_TTL)
                .is_ok()
        );
    }

    fn valid_entry(name: &str, expires_at: Instant) -> CacheEntry {
        CacheEntry::Valid {
            identity: PlatformIdentity::Shared {
                name: name.to_owned(),
            },
            expires_at,
        }
    }

    #[test]
    fn capacity_bound_evicts_soonest_to_expire_when_full() {
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();
        let now = Instant::now();

        for i in 0..MAX_CACHE_ENTRIES {
            let name = format!("tok-{i}");
            // Ascending expiry: `tok-0` expires soonest.
            let expires_at = now + Duration::from_mins(1) + Duration::from_micros(i as u64);
            cached.insert(name.clone(), valid_entry(&name, expires_at), now);
        }
        assert_eq!(cached.cache.lock().len(), MAX_CACHE_ENTRIES);

        cached.insert(
            "overflow".to_owned(),
            valid_entry("overflow", now + Duration::from_mins(2)),
            now,
        );

        let cache = cached.cache.lock();
        assert_eq!(
            cache.len(),
            MAX_CACHE_ENTRIES,
            "cache must never grow past MAX_CACHE_ENTRIES"
        );
        assert!(
            cache.contains_key("overflow"),
            "the newly inserted token must be present"
        );
        assert!(
            !cache.contains_key("tok-0"),
            "the soonest-to-expire entry must be evicted to make room"
        );
    }

    #[test]
    fn full_cache_reclaims_expired_before_scanning_for_a_victim() {
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();
        let now = Instant::now();

        // Fill one short of capacity with valid, far-future entries: the
        // soonest-to-expire scan would pick one of these if an expired entry
        // were not reclaimed first.
        for i in 1..MAX_CACHE_ENTRIES {
            let name = format!("tok-{i}");
            let expires_at = now + Duration::from_mins(5) + Duration::from_micros(i as u64);
            cached.insert(name.clone(), valid_entry(&name, expires_at), now);
        }
        // Insert the already-expired entry last so a periodic sweep during the
        // fill loop above cannot reclaim it before the capacity path runs.
        cached.insert(
            "expired".to_owned(),
            valid_entry("expired", now.checked_sub(Duration::from_secs(1)).unwrap()),
            now,
        );
        assert_eq!(cached.cache.lock().len(), MAX_CACHE_ENTRIES);

        cached.insert(
            "overflow".to_owned(),
            valid_entry("overflow", now + Duration::from_mins(10)),
            now,
        );

        let cache = cached.cache.lock();
        assert_eq!(cache.len(), MAX_CACHE_ENTRIES);
        assert!(cache.contains_key("overflow"));
        assert!(
            !cache.contains_key("expired"),
            "the expired entry must be reclaimed by the sweep, making room"
        );
        assert!(
            cache.contains_key("tok-1"),
            "a still-valid entry must not be evicted while an expired one exists"
        );
    }

    #[tokio::test]
    async fn concurrent_misses_for_the_same_token_single_flight() {
        let cached = Arc::new(
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap(),
        );

        let mut handles = Vec::new();
        for _ in 0..8 {
            let cached = Arc::clone(&cached);
            handles.push(tokio::spawn(
                async move { cached.authenticate("burst").await },
            ));
        }
        for handle in handles {
            handle.await.unwrap().unwrap();
        }

        assert_eq!(
            cached.inner.calls(),
            1,
            "a burst of concurrent misses for the same token must collapse to one backend call"
        );
    }

    #[tokio::test]
    async fn aborting_an_inflight_call_still_releases_the_single_flight_slot() {
        let auth = CountingAuth::new();
        auth.set_delay(Duration::from_millis(200));
        let cached =
            Arc::new(CachingInternalAuthenticator::new(auth, Duration::from_mins(1)).unwrap());

        let handle = {
            let cached = Arc::clone(&cached);
            tokio::spawn(async move { cached.authenticate("burst").await })
        };
        // Give the task time to take the single-flight lock and start
        // blocking on the (delayed) backend call before aborting it.
        tokio::time::sleep(Duration::from_millis(20)).await;
        handle.abort();
        let result = handle.await;
        assert!(
            result.unwrap_err().is_cancelled(),
            "the task must actually have been aborted mid-flight for this test to be meaningful"
        );

        assert!(
            cached.inflight.lock().is_empty(),
            "aborting an in-flight call must not leak its single-flight slot"
        );

        // The slot must also be fully usable afterwards: a fresh call for the
        // same token must not deadlock on a lock nobody will ever release.
        let identity = tokio::time::timeout(Duration::from_secs(1), cached.authenticate("burst"))
            .await
            .expect("post-abort call must not hang")
            .unwrap();
        assert_eq!(identity.peer_name(), "burst");
    }

    #[test]
    fn clamped_expiry_never_extends_trust_past_an_expired_jwt() {
        let now = Instant::now();
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"exp":1}"#);
        let expired_token = format!("h.{payload}.s");
        assert_eq!(
            clamped_expiry(&expired_token, now, Duration::from_mins(1)),
            now,
            "a JWT already expired per its own claim must not be trusted at all"
        );
    }

    #[test]
    fn clamped_expiry_uses_jwt_remaining_life_when_shorter_than_ttl() {
        let now = Instant::now();
        let exp = (SystemTime::now() + Duration::from_secs(2))
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#));
        let token = format!("h.{payload}.s");
        let expiry = clamped_expiry(&token, now, Duration::from_mins(5));
        assert!(
            expiry < now + Duration::from_mins(5),
            "a JWT with less remaining life than the configured ttl must clamp to the JWT's expiry"
        );
    }

    #[tokio::test]
    async fn second_waiter_reuses_result_populated_while_it_waited() {
        let auth = CountingAuth::new();
        auth.set_delay(Duration::from_millis(50));
        let cached =
            Arc::new(CachingInternalAuthenticator::new(auth, Duration::from_mins(1)).unwrap());

        let first = {
            let cached = Arc::clone(&cached);
            tokio::spawn(async move { cached.authenticate("burst").await })
        };
        // Give the first call time to take the single-flight lock and start
        // its (delayed) backend call, so this call genuinely blocks on the
        // lock instead of racing it.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let second = cached.authenticate("burst").await.unwrap();
        let first = first.await.unwrap().unwrap();

        assert_eq!(first, second);
        assert_eq!(
            cached.inner.calls(),
            1,
            "a waiter that blocked on the single-flight lock must reuse the result the winner stored"
        );
    }

    #[tokio::test]
    async fn second_waiter_reuses_rejection_populated_while_it_waited() {
        let auth = CountingAuth::new();
        auth.set_delay(Duration::from_millis(50));
        auth.set_mode(Mode::Invalid);
        let cached =
            Arc::new(CachingInternalAuthenticator::new(auth, Duration::from_mins(1)).unwrap());

        let first = {
            let cached = Arc::clone(&cached);
            tokio::spawn(async move { cached.authenticate("burst").await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        let second = cached.authenticate("burst").await;
        let first = first.await.unwrap();

        assert!(matches!(first, Err(InternalAuthNError::InvalidToken)));
        assert!(matches!(second, Err(InternalAuthNError::InvalidToken)));
        assert_eq!(
            cached.inner.calls(),
            1,
            "a waiter that blocked on the single-flight lock must reuse the cached rejection"
        );
    }

    #[tokio::test]
    async fn lock_waiter_revalidates_an_entry_that_expired_during_the_wait() {
        let auth = CountingAuth::new();
        // Widen the single-flight window so the second caller genuinely blocks
        // on the per-token lock across the winner's backend round-trip.
        auth.set_delay(Duration::from_millis(50));
        let cached =
            Arc::new(CachingInternalAuthenticator::new(auth, Duration::from_mins(1)).unwrap());

        // An already-expired JWT: the backend accepts it, but `clamped_expiry`
        // stores it with `expires_at == stored_at` — expired the instant it
        // lands. A waiter that re-checked with its pre-wait `now` (sampled
        // before `stored_at`) would wrongly serve it; the refreshed cutoff must
        // treat it as a miss and re-validate.
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"exp":1}"#);
        let token = format!("h.{payload}.s");

        let first = {
            let cached = Arc::clone(&cached);
            let token = token.clone();
            tokio::spawn(async move { cached.authenticate(&token).await })
        };
        // Let the winner take the lock and start its (delayed) backend call so
        // this caller blocks on the lock rather than racing it.
        tokio::time::sleep(Duration::from_millis(10)).await;
        cached.authenticate(&token).await.unwrap();
        first.await.unwrap().unwrap();

        assert_eq!(
            cached.inner.calls(),
            2,
            "a waiter must re-validate an entry that expired during its wait, not serve it stale"
        );
    }

    #[test]
    fn jwt_exp_claim_extracts_and_ignores_non_jwt() {
        // header.payload.signature, payload = {"exp":123} base64url (no padding).
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"exp":123}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(jwt_exp_claim(&token), Some(123));

        assert_eq!(jwt_exp_claim("not-a-jwt"), None);
        assert_eq!(jwt_exp_claim("shared-secret-token"), None);
    }
}
