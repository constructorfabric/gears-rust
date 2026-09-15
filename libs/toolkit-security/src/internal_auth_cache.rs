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

use std::collections::{BTreeSet, HashMap};
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

/// Longest credential this cache will hold as a key.
///
/// [`MAX_CACHE_ENTRIES`] bounds how *many* entries exist, but not how large any
/// one of them is, and a rejected credential is attacker-supplied in full. A
/// stream of distinct oversized junk tokens could therefore pin
/// `MAX_CACHE_ENTRIES` header-sized strings at once, with the only real bound
/// being whatever limit the transport happened to enforce.
///
/// Comfortably above any real JWT — those run to a few kilobytes even with
/// generous claims — so this bounds abuse without turning a legitimate
/// credential away. A token over the limit is not rejected, it simply bypasses
/// the cache: the memory stays bounded and the verdict still comes from the
/// backend.
const MAX_CACHEABLE_TOKEN_LEN: usize = 16 * 1024;

/// How long a single backend validation may take before it is abandoned.
///
/// The backend call happens while the per-token single-flight lock is held, so
/// a backend that never answers would otherwise park every caller presenting
/// that token for as long as it stays unresponsive — with no deadline, since
/// the waiters are blocked on the lock rather than on the call. Bounding the
/// call converts an indefinite hang into an `Unavailable` a caller can act on.
///
/// This is a request deadline, deliberately unrelated to the cache TTL: it is
/// sized for how long a `TokenReview` round-trip may reasonably take, not for
/// how long its answer stays usable.
pub const TOKEN_REVIEW_BACKEND_TIMEOUT: Duration = Duration::from_secs(10);

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

/// The cache map, paired with an index of its entries ordered by expiry.
///
/// The index exists so that neither reclaiming expired entries nor choosing an
/// eviction victim has to scan the map. That matters because every one of those
/// operations runs under the cache mutex, on the authentication hot path: the
/// previous whole-map `min_by_key` scan ran on *every* insert for as long as the
/// cache stayed full of still-valid entries, which is exactly the sustained-load
/// case, and it blocked every concurrent lookup while it ran.
///
/// The two collections are only consistent if they are updated together, so the
/// map is private and every mutation goes through a method here.
struct ExpiringCache {
    entries: HashMap<String, CacheEntry>,
    /// `(expires_at, token)` for every entry in `entries`. Ordered, so the
    /// soonest-to-expire is the first element and expired entries are a prefix.
    by_expiry: BTreeSet<(Instant, String)>,
}

impl ExpiringCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            by_expiry: BTreeSet::new(),
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn contains_key(&self, token: &str) -> bool {
        self.entries.contains_key(token)
    }

    fn get(&self, token: &str) -> Option<&CacheEntry> {
        self.entries.get(token)
    }

    fn insert(&mut self, token: String, entry: CacheEntry) {
        let expires_at = entry.expires_at();
        if let Some(previous) = self.entries.insert(token.clone(), entry) {
            self.by_expiry.remove(&(previous.expires_at(), token.clone()));
        }
        self.by_expiry.insert((expires_at, token));
    }

    fn remove(&mut self, token: &str) {
        if let Some(entry) = self.entries.remove(token) {
            self.by_expiry.remove(&(entry.expires_at(), token.to_owned()));
        }
    }

    /// Drop every entry that has expired by `now`. Touches only the entries it
    /// removes: they are a prefix of the index.
    fn sweep_expired(&mut self, now: Instant) {
        // `Instant` has no "minimum", so split at `now` instead: everything
        // ordered before `(now, "")` expired at or before it.
        let live = self.by_expiry.split_off(&(now, String::new()));
        let expired = std::mem::replace(&mut self.by_expiry, live);
        for (_, token) in expired {
            self.entries.remove(&token);
        }
    }

    /// Drop the entry that expires soonest, which is the one needing
    /// re-validation soonest anyway. `O(log n)`, no scan.
    fn evict_soonest(&mut self) {
        if let Some((expires_at, token)) = self.by_expiry.pop_first() {
            self.entries.remove(&token);
            debug_assert!(
                !self.entries.contains_key(&token),
                "index and map disagreed about {token} at {expires_at:?}"
            );
        }
    }
}

/// What a credential's `exp` claim says, if anything.
///
/// An `Option<u64>` collapsed two different answers onto `None`: a credential
/// carrying no `exp` because it is not a JWT, and a JWT whose `exp` could not
/// be read. The first is cacheable for the full TTL; the second is not, because
/// the one thing known about it is that it declares an expiry we cannot honour.
enum ExpClaim {
    /// Not a JWT (e.g. a shared secret). Nothing to clamp against.
    NotJwt,
    /// A JWT declaring this expiry, in seconds since the Unix epoch.
    Expires(u64),
    /// Shaped like a JWT, but `exp` is absent, not a number, or the payload did
    /// not decode.
    Unreadable,
}

/// Best-effort extraction of the `exp` (seconds since the Unix epoch) claim
/// from a JWT, without verifying the signature.
///
/// The caller has already had the token's signature verified by the
/// authentication backend (e.g. Kubernetes `TokenReview`); this is a plain
/// base64 decode of the already-trusted payload, used only to avoid caching a
/// validation past the credential's own expiry.
fn jwt_exp_claim(token: &str) -> ExpClaim {
    // Three dot-separated parts is what makes this a JWT rather than an opaque
    // credential, and an opaque credential has no expiry to honour.
    let mut parts = token.split('.');
    let (Some(_header), Some(payload_b64), Some(_signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return ExpClaim::NotJwt;
    };

    let Ok(payload) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64) else {
        return ExpClaim::Unreadable;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return ExpClaim::Unreadable;
    };
    match value.get("exp").and_then(serde_json::Value::as_u64) {
        Some(exp) => ExpClaim::Expires(exp),
        None => ExpClaim::Unreadable,
    }
}

/// The instant a freshly validated `token` should stop being trusted from
/// cache: whichever is sooner of `now + ttl` and the token's own `exp` claim
/// (when present).
///
/// Every arithmetic step is checked. `exp` comes out of a base64 payload with
/// no bound on its value, and both `SystemTime + Duration` and
/// `Instant + Duration` panic on overflow — which would take down the platform
/// authentication path rather than skip a clamp.
fn clamped_expiry(token: &str, now: Instant, ttl: Duration) -> Instant {
    let ttl_expiry = now.checked_add(ttl).unwrap_or(now);
    let exp_secs = match jwt_exp_claim(token) {
        ExpClaim::NotJwt => return ttl_expiry,
        // A declared-but-unreadable expiry is not a licence to cache for the
        // full TTL: expire immediately and re-validate on the next call.
        ExpClaim::Unreadable => return now,
        ExpClaim::Expires(exp) => exp,
    };

    let Some(exp_at) = UNIX_EPOCH.checked_add(Duration::from_secs(exp_secs)) else {
        // An `exp` too far in the future to fit a `SystemTime` says nothing
        // useful about when to stop trusting the token; fall back to the TTL.
        return ttl_expiry;
    };
    let Ok(remaining) = exp_at.duration_since(SystemTime::now()) else {
        // The token's own claim says it is already expired; do not extend
        // trust in it at all.
        return now;
    };
    let exp_expiry = now.checked_add(remaining).unwrap_or(ttl_expiry);
    ttl_expiry.min(exp_expiry)
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
    cache: Mutex<ExpiringCache>,
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
            cache: Mutex::new(ExpiringCache::new()),
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
            cache: Mutex::new(ExpiringCache::new()),
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
    /// [`SWEEP_INTERVAL`] inserts and enforcing [`MAX_CACHE_ENTRIES`] when a
    /// new token would exceed it.
    ///
    /// Both the sweep and the eviction go through [`ExpiringCache`]'s expiry
    /// index, so each touches only the entries it actually removes. Under
    /// sustained load with a cache full of still-valid entries — the case that
    /// matters, since TTL expiry alone never reclaims anything there — this is
    /// the difference between a whole-map scan per insert and an `O(log n)`
    /// lookup, all of it under the mutex every concurrent lookup needs.
    fn insert(&self, token: String, entry: CacheEntry, now: Instant) {
        let mut cache = self.cache.lock();
        let count = self.sweep_counter.fetch_add(1, Ordering::Relaxed) + 1;
        if count.is_multiple_of(SWEEP_INTERVAL) {
            cache.sweep_expired(now);
        }
        if cache.len() >= MAX_CACHE_ENTRIES && !cache.contains_key(&token) {
            // Reclaim what has expired before evicting anything still valid.
            cache.sweep_expired(now);
            if cache.len() >= MAX_CACHE_ENTRIES {
                // Every entry is still valid, so caching is now costing us a
                // live entry per insert and the backend sees a call for each
                // one evicted. Nothing else reports that, and the symptom
                // downstream is a surge of TokenReview traffic that looks like
                // a backend problem rather than cache saturation.
                tracing::warn!(
                    entries = cache.len(),
                    max_entries = MAX_CACHE_ENTRIES,
                    "internal-auth cache is full of unexpired entries; evicting a live entry"
                );
                cache.evict_soonest();
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
        // Oversized credentials never enter the map, in either direction: the
        // key is attacker-supplied and only the entry *count* is bounded. They
        // still get a verdict, just not a cached one.
        if token.len() > MAX_CACHEABLE_TOKEN_LEN {
            return self.inner.authenticate(token).await;
        }

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

        // Bounded: `_guard` is held across this await, so an unresponsive
        // backend would otherwise block every caller presenting this token
        // indefinitely — they are parked on the lock, not on a call that could
        // time out on its own.
        let result = match tokio::time::timeout(
            TOKEN_REVIEW_BACKEND_TIMEOUT,
            self.inner.authenticate(token),
        )
        .await
        {
            Ok(result) => result,
            // Not cached either way: a timeout says nothing about whether the
            // credential is valid, and caching it would turn one slow call into
            // a fixed window of denials.
            Err(_elapsed) => return Err(InternalAuthNError::Unavailable),
        };
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
    async fn an_oversized_token_never_enters_the_cache() {
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();
        let huge = "x".repeat(MAX_CACHEABLE_TOKEN_LEN + 1);

        // Still authenticated, twice, because nothing was cached.
        cached.authenticate(&huge).await.unwrap();
        cached.authenticate(&huge).await.unwrap();

        assert_eq!(
            cached.cache.lock().len(),
            0,
            "an oversized token must not become a cache key: the key is \
             attacker-supplied and only the entry count is bounded"
        );
        assert_eq!(
            cached.inner.calls(),
            2,
            "bypassing the cache means every call reaches the backend"
        );
    }

    #[test]
    fn the_expiry_index_tracks_the_map_through_overwrites_and_removals() {
        // The map and its index are only useful if they agree; an entry left in
        // the index after its map entry is gone would evict the wrong token.
        let mut cache = ExpiringCache::new();
        let now = Instant::now();

        cache.insert("a".to_owned(), valid_entry("a", now + Duration::from_secs(30)));
        cache.insert("b".to_owned(), valid_entry("b", now + Duration::from_secs(10)));
        // Overwrite `a` with a *sooner* expiry: the stale index entry must go,
        // or `a` would look like it expires at the original, later instant.
        cache.insert("a".to_owned(), valid_entry("a", now + Duration::from_secs(5)));
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.by_expiry.len(), 2, "index must not keep a stale entry");

        cache.evict_soonest();
        assert!(!cache.contains_key("a"), "`a` now expires soonest");
        assert!(cache.contains_key("b"));
        assert_eq!(cache.by_expiry.len(), 1);

        cache.remove("b");
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.by_expiry.len(), 0);
    }

    #[test]
    fn sweeping_removes_exactly_the_expired() {
        let mut cache = ExpiringCache::new();
        let now = Instant::now();

        cache.insert(
            "expired".to_owned(),
            valid_entry("expired", now.checked_sub(Duration::from_secs(1)).unwrap()),
        );
        cache.insert("live".to_owned(), valid_entry("live", now + Duration::from_mins(1)));

        cache.sweep_expired(now);

        assert!(!cache.contains_key("expired"));
        assert!(cache.contains_key("live"));
        assert_eq!(
            cache.by_expiry.len(),
            1,
            "the index must shrink with the map"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_backend_times_out_rather_than_parking_every_caller() {
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();
        // Longer than the deadline, i.e. a backend that never answers.
        cached
            .inner
            .set_delay(TOKEN_REVIEW_BACKEND_TIMEOUT * 2);

        let err = cached
            .authenticate("tok")
            .await
            .expect_err("a backend that outlasts the deadline must not resolve");
        assert!(
            matches!(err, InternalAuthNError::Unavailable),
            "a timeout is a backend availability problem, not a verdict on the credential"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_validation_is_not_cached() {
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();
        cached
            .inner
            .set_delay(TOKEN_REVIEW_BACKEND_TIMEOUT * 2);
        drop(cached.authenticate("tok").await);

        // The backend recovers; the next call must reach it rather than serve a
        // cached failure, or one slow call would deny the token for a full TTL.
        cached.inner.set_delay(Duration::ZERO);
        let identity = cached
            .authenticate("tok")
            .await
            .expect("a recovered backend must be consulted again");
        assert_eq!(identity.peer_name(), "tok");
        assert_eq!(
            cached.inner.calls(),
            2,
            "the timed-out attempt must not have been cached"
        );
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
        let t0 = Instant::now();

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

        // Step past the negative-cache window with the injected instant rather
        // than a real sleep: this cache keys off `std::time::Instant`, which
        // `tokio::time::pause` cannot virtualize, so a sleep here would burn a
        // real second and still drift on a loaded runner.
        cached.inner.set_mode(Mode::Succeed);
        let identity = cached
            .authenticate_at("bad", t0 + NEGATIVE_CACHE_TTL + Duration::from_millis(1))
            .await
            .unwrap();
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

        // Bounded from both sides. An upper bound alone holds for any value
        // below the ttl, `now` included, so a clamp that collapsed to zero --
        // disabling positive caching entirely -- would still pass.
        assert!(
            expiry > now + Duration::from_secs(1),
            "the token has ~2s of life left; clamping to less would disable caching"
        );
        assert!(
            expiry <= now + Duration::from_secs(2),
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

    /// Build `header.payload.signature` around a base64url payload.
    fn jwt_with_payload(payload: &[u8]) -> String {
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        format!("h.{encoded}.s")
    }

    #[test]
    fn jwt_exp_claim_extracts_and_ignores_non_jwt() {
        let token = jwt_with_payload(br#"{"exp":123}"#);
        assert!(matches!(jwt_exp_claim(&token), ExpClaim::Expires(123)));

        assert!(matches!(jwt_exp_claim("not-a-jwt"), ExpClaim::NotJwt));
        assert!(matches!(
            jwt_exp_claim("shared-secret-token"),
            ExpClaim::NotJwt
        ));
    }

    #[test]
    fn jwt_exp_claim_separates_unreadable_from_absent() {
        // The distinction that matters: a credential with no expiry to honour
        // is cacheable for the full TTL, one that declares an unreadable expiry
        // is not.
        assert!(matches!(
            jwt_exp_claim(&jwt_with_payload(br"{}")),
            ExpClaim::Unreadable
        ));
        assert!(matches!(
            jwt_exp_claim(&jwt_with_payload(br#"{"exp":"soon"}"#)),
            ExpClaim::Unreadable
        ));
        assert!(matches!(
            jwt_exp_claim(&jwt_with_payload(br#"{"exp":1.5}"#)),
            ExpClaim::Unreadable
        ));
        assert!(matches!(
            jwt_exp_claim(&jwt_with_payload(br"not json")),
            ExpClaim::Unreadable
        ));
        assert!(matches!(
            jwt_exp_claim("h.!!!not-base64!!!.s"),
            ExpClaim::Unreadable
        ));
    }

    #[test]
    fn unreadable_exp_is_not_cached_for_the_full_ttl() {
        let now = Instant::now();
        let ttl = Duration::from_mins(5);

        // No expiry declared at all: the TTL applies.
        assert_eq!(
            clamped_expiry("shared-secret-token", now, ttl),
            now + ttl,
            "an opaque credential has no expiry to clamp against"
        );

        // An expiry we cannot read: expire immediately rather than trust it for
        // five minutes.
        let unreadable = jwt_with_payload(br#"{"exp":"soon"}"#);
        assert_eq!(
            clamped_expiry(&unreadable, now, ttl),
            now,
            "a declared-but-unreadable expiry must not be cached for the full TTL"
        );
    }

    #[test]
    fn absurd_exp_claims_do_not_panic() {
        let now = Instant::now();
        let ttl = Duration::from_secs(30);

        // `SystemTime + Duration` and `Instant + Duration` both panic on
        // overflow, and `exp` is attacker-influenced.
        for payload in [
            format!(r#"{{"exp":{}}}"#, u64::MAX),
            format!(r#"{{"exp":{}}}"#, i64::MAX),
            r#"{"exp":0}"#.to_owned(),
        ] {
            let token = jwt_with_payload(payload.as_bytes());
            let expiry = clamped_expiry(&token, now, ttl);
            assert!(
                expiry <= now + ttl,
                "the clamp must never extend trust beyond the configured TTL"
            );
        }
    }
}
