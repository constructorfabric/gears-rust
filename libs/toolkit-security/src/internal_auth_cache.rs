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
//! - The cache key is a SHA-256 digest of the token, so no map here holds the
//!   credential itself and an entry costs the same whatever the token's
//!   length. A cryptographic digest rather than a fast hash because the input
//!   is a credential: a collision would let one token answer for another.
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

/// Default deadline for the whole [`InternalAuthenticator::authenticate`]
/// call, overridable per instance with
/// [`CachingInternalAuthenticator::with_authentication_timeout`].
///
/// The name says "authentication", not "backend": the deadline covers the
/// per-token single-flight lock wait as well as the backend round-trip. A
/// backend call alone would not have been enough -- three concurrent callers
/// presenting the same token against a hung backend each started their own
/// timer only on reaching the front of the queue, so they finished 1x, 2x and
/// 3x the deadline apart instead of each failing within one deadline of its
/// own arrival. Bounding the whole call converts an indefinite hang, or an
/// indefinite queue, into an `Unavailable` every caller can act on within the
/// same window.
///
/// A request deadline, deliberately unrelated to the cache TTL: it is sized
/// for how long a `TokenReview` round-trip (plus queueing behind it) may
/// reasonably take, not for how long a cached answer stays usable.
pub const DEFAULT_AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(10);

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

/// A cache key: the SHA-256 digest of a credential, never the credential.
///
/// The map, the expiry index and the single-flight map are all keyed on this.
/// Keying them on the token itself stored the credential two or three times
/// over — at `MAX_CACHE_ENTRIES` entries and a header-sized token that ran to
/// hundreds of megabytes per cache instance — and kept the plaintext alive for
/// as long as the entry did. A digest is 32 bytes whatever the token's length,
/// which also removes the only reason the cache had to refuse long tokens.
///
/// A cryptographic digest rather than a fast hash: the input is a credential,
/// and a collision here would let one token answer for another.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct TokenKey([u8; 32]);

impl TokenKey {
    fn of(token: &str) -> Self {
        use sha2::{Digest as _, Sha256};
        Self(Sha256::digest(token.as_bytes()).into())
    }
}

impl std::fmt::Debug for TokenKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Enough to correlate two log lines, not enough to be worth anything to
        // whoever reads them.
        write!(f, "TokenKey({:02x}{:02x}..)", self.0[0], self.0[1])
    }
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
    entries: HashMap<TokenKey, CacheEntry>,
    /// `(expires_at, token)` for every entry in `entries`. Ordered, so the
    /// soonest-to-expire is the first element and expired entries are a prefix.
    by_expiry: BTreeSet<(Instant, TokenKey)>,
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

    fn contains_key(&self, key: &TokenKey) -> bool {
        self.entries.contains_key(key)
    }

    fn get(&self, key: &TokenKey) -> Option<&CacheEntry> {
        self.entries.get(key)
    }

    fn insert(&mut self, key: TokenKey, entry: CacheEntry) {
        let expires_at = entry.expires_at();
        if let Some(previous) = self.entries.insert(key, entry) {
            self.by_expiry.remove(&(previous.expires_at(), key));
        }
        self.by_expiry.insert((expires_at, key));
    }

    fn remove(&mut self, key: &TokenKey) {
        if let Some(entry) = self.entries.remove(key) {
            self.by_expiry.remove(&(entry.expires_at(), *key));
        }
    }

    /// Drop every entry that has expired by `now`. Touches only the entries it
    /// removes: they are a prefix of the index.
    fn sweep_expired(&mut self, now: Instant) {
        // `Instant` has no "minimum", so split at `now` instead: everything
        // ordered before `(now, <all-zero key>)` expired at or before it.
        let live = self.by_expiry.split_off(&(now, TokenKey([0; 32])));
        let expired = std::mem::replace(&mut self.by_expiry, live);
        for (_, key) in expired {
            self.entries.remove(&key);
        }
    }

    /// Drop the entry that expires soonest, which is the one needing
    /// re-validation soonest anyway. `O(log n)`, no scan.
    fn evict_soonest(&mut self) {
        if let Some((expires_at, key)) = self.by_expiry.pop_first() {
            self.entries.remove(&key);
            debug_assert!(
                !self.entries.contains_key(&key),
                "index and map disagreed about {key:?} at {expires_at:?}"
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
    /// Deadline for the whole `authenticate` call. Defaults to
    /// [`DEFAULT_AUTHENTICATION_TIMEOUT`]; override with
    /// [`with_authentication_timeout`](Self::with_authentication_timeout).
    authentication_timeout: Duration,
    cache: Mutex<ExpiringCache>,
    /// Per-token single-flight locks: concurrent misses for the same token
    /// serialize here instead of each issuing a backend call.
    inflight: Mutex<HashMap<TokenKey, Arc<AsyncMutex<()>>>>,
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
            authentication_timeout: DEFAULT_AUTHENTICATION_TIMEOUT,
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
            authentication_timeout: DEFAULT_AUTHENTICATION_TIMEOUT,
            cache: Mutex::new(ExpiringCache::new()),
            inflight: Mutex::new(HashMap::new()),
            sweep_counter: AtomicU32::new(0),
        }
    }

    /// Override the deadline applied to the whole `authenticate` call
    /// (including the per-token lock wait), replacing
    /// [`DEFAULT_AUTHENTICATION_TIMEOUT`].
    #[must_use]
    pub fn with_authentication_timeout(mut self, timeout: Duration) -> Self {
        self.authentication_timeout = timeout;
        self
    }

    /// Look up a still-applicable cached outcome for `token`, evicting it
    /// immediately if found stale (rather than waiting for the next sweep).
    ///
    /// Holds the lock only for the duration of the map access (never across
    /// an `await`), so the returned future stays `Send`.
    fn lookup(&self, key: &TokenKey, now: Instant) -> CacheLookup {
        let mut cache = self.cache.lock();
        let Some(entry) = cache.get(key) else {
            return CacheLookup::Miss;
        };
        if entry.expires_at() <= now {
            cache.remove(key);
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
    fn insert(&self, key: TokenKey, entry: CacheEntry, now: Instant) {
        let mut cache = self.cache.lock();
        let count = self.sweep_counter.fetch_add(1, Ordering::Relaxed) + 1;
        if count.is_multiple_of(SWEEP_INTERVAL) {
            cache.sweep_expired(now);
        }
        if cache.len() >= MAX_CACHE_ENTRIES && !cache.contains_key(&key) {
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
        cache.insert(key, entry);
    }

    /// Get-or-create the per-token single-flight lock.
    fn token_lock(&self, key: TokenKey) -> Arc<AsyncMutex<()>> {
        let mut inflight = self.inflight.lock();
        Arc::clone(
            inflight
                .entry(key)
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    }

    /// Drop the per-token lock from the map once nothing else references it,
    /// so `inflight` does not grow unboundedly over the process lifetime.
    fn release_token_lock(&self, key: &TokenKey, lock: &Arc<AsyncMutex<()>>) {
        let mut inflight = self.inflight.lock();
        // 2 = the map's own clone + `lock` here; anything higher means
        // another waiter still holds a clone.
        if Arc::strong_count(lock) <= 2 {
            inflight.remove(key);
        }
    }
}

/// Releases the `inflight` entry on drop, so cancellation (not just a normal
/// return) still cleans it up. Borrows `lock` rather than cloning it, so it
/// doesn't skew `release_token_lock`'s `Arc::strong_count` check.
struct ReleaseTokenLockOnDrop<'a, A> {
    owner: &'a CachingInternalAuthenticator<A>,
    key: TokenKey,
    lock: &'a Arc<AsyncMutex<()>>,
}

impl<A> Drop for ReleaseTokenLockOnDrop<'_, A> {
    fn drop(&mut self) {
        self.owner.release_token_lock(&self.key, self.lock);
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
        // Every map is keyed on the digest, so the credential's length no longer
        // decides what the cache costs and there is nothing to bypass.
        let key = TokenKey::of(token);

        match self.lookup(&key, now) {
            CacheLookup::Valid(identity) => return Ok(identity),
            CacheLookup::Rejected => return Err(InternalAuthNError::InvalidToken),
            CacheLookup::Miss => {}
        }

        // Single-flight: serialize concurrent misses for the same token so a
        // burst of calls collapses into one backend round-trip.
        let lock = self.token_lock(key);
        // Constructed *before* the lock is acquired, not after: a caller
        // cancelled while still waiting for the lock -- not yet holding it --
        // must still release its `inflight` registration. With the guard built
        // only after a successful `.lock().await`, a waiter cancelled before
        // that point had no guard at all, so its registration was never
        // cleaned up and outlived every other reference to it: an unbounded,
        // per-cancelled-token leak in `inflight`.
        let _release = ReleaseTokenLockOnDrop {
            owner: self,
            key,
            lock: &lock,
        };
        let _guard = lock.lock().await;

        // Another caller may have populated the cache while this one waited.
        // Refresh the cutoff: `now` predates the lock wait, so a stale value
        // could serve an entry that expired during it. Later of the injected
        // instant and real time keeps a test's deterministic `now` dominant.
        let now = now.max(Instant::now());
        match self.lookup(&key, now) {
            CacheLookup::Valid(identity) => return Ok(identity),
            CacheLookup::Rejected => return Err(InternalAuthNError::InvalidToken),
            CacheLookup::Miss => {}
        }

        // Unbounded here on purpose: the deadline lives in `authenticate`, so
        // it covers the wait for the lock above as well as this call. A second
        // timeout here would restart the clock for whoever just acquired the
        // lock, which is exactly the queueing behaviour the outer one removes.
        //
        // A caller abandoned at the deadline simply drops this future; nothing
        // is cached for it, which is right — a timeout says nothing about
        // whether the credential is valid, and caching it would turn one slow
        // call into a fixed window of denials.
        let result = self.inner.authenticate(token).await;
        // Re-sampled *after* the backend round-trip: sampling before it would
        // shrink the effective TTL by however long the call took.
        let stored_at = Instant::now();

        match result {
            Ok(identity) => {
                let expires_at = clamped_expiry(token, stored_at, self.ttl);
                self.insert(
                    key,
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
                    key,
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
        // One deadline for the whole call, measured from *this* caller's
        // arrival, and it covers the wait for the per-token lock as well as the
        // backend round-trip.
        //
        // Bounding only the backend call was not enough: the lock is taken
        // first, so three concurrent callers presenting the same token against
        // a hung backend each started their own timer on reaching the front of
        // the queue and finished at roughly 10s, 20s and 30s. Now each of them
        // fails within one deadline of arriving.
        //
        // Cancelling here is safe: the lock guard and `ReleaseTokenLockOnDrop`
        // both clean up on drop, so an abandoned attempt leaves no lock held
        // and no `inflight` entry behind.
        match tokio::time::timeout(
            self.authentication_timeout,
            self.authenticate_at(token, Instant::now()),
        )
        .await
        {
            Ok(result) => result,
            Err(_elapsed) => Err(InternalAuthNError::Unavailable),
        }
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
    async fn a_very_long_token_is_cached_like_any_other() {
        // It used to bypass the cache, because the token *was* the key and a
        // header-sized one cost that much memory per entry. The key is a digest
        // now, so length no longer decides anything.
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();
        let huge = "x".repeat(64 * 1024);

        cached.authenticate(&huge).await.unwrap();
        cached.authenticate(&huge).await.unwrap();

        assert_eq!(cached.cache.lock().len(), 1);
        assert_eq!(
            cached.inner.calls(),
            1,
            "the second call must be served from cache"
        );
    }

    #[test]
    fn the_cache_key_is_a_fixed_size_digest_of_the_token() {
        // The property the memory bound rests on: two tokens of wildly
        // different length produce keys of the same size, and distinct tokens
        // produce distinct keys.
        let short = TokenKey::of("t");
        let long = TokenKey::of(&"x".repeat(64 * 1024));

        assert_eq!(short.0.len(), long.0.len());
        assert_ne!(short, long);
        assert_eq!(
            short,
            TokenKey::of("t"),
            "the same token must map to itself"
        );

        // And it does not render the credential.
        assert!(!format!("{:?}", TokenKey::of("super-secret")).contains("super-secret"));
    }

    #[test]
    fn the_expiry_index_tracks_the_map_through_overwrites_and_removals() {
        // The map and its index are only useful if they agree; an entry left in
        // the index after its map entry is gone would evict the wrong token.
        let mut cache = ExpiringCache::new();
        let now = Instant::now();

        cache.insert(
            TokenKey::of("a"),
            valid_entry("a", now + Duration::from_secs(30)),
        );
        cache.insert(
            TokenKey::of("b"),
            valid_entry("b", now + Duration::from_secs(10)),
        );
        // Overwrite `a` with a *sooner* expiry: the stale index entry must go,
        // or `a` would look like it expires at the original, later instant.
        cache.insert(
            TokenKey::of("a"),
            valid_entry("a", now + Duration::from_secs(5)),
        );
        assert_eq!(cache.len(), 2);
        assert_eq!(
            cache.by_expiry.len(),
            2,
            "index must not keep a stale entry"
        );

        cache.evict_soonest();
        assert!(
            !cache.contains_key(&TokenKey::of("a")),
            "`a` now expires soonest"
        );
        assert!(cache.contains_key(&TokenKey::of("b")));
        assert_eq!(cache.by_expiry.len(), 1);

        cache.remove(&TokenKey::of("b"));
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.by_expiry.len(), 0);
    }

    #[test]
    fn sweeping_removes_exactly_the_expired() {
        let mut cache = ExpiringCache::new();
        let now = Instant::now();

        cache.insert(
            TokenKey::of("expired"),
            valid_entry("expired", now.checked_sub(Duration::from_secs(1)).unwrap()),
        );
        cache.insert(
            TokenKey::of("live"),
            valid_entry("live", now + Duration::from_mins(1)),
        );

        cache.sweep_expired(now);

        assert!(!cache.contains_key(&TokenKey::of("expired")));
        assert!(cache.contains_key(&TokenKey::of("live")));
        assert_eq!(
            cache.by_expiry.len(),
            1,
            "the index must shrink with the map"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn with_authentication_timeout_overrides_the_default() {
        // A backend slower than the *shortened* deadline must still time out,
        // even though it would comfortably fit inside the default 10s.
        let short = Duration::from_millis(50);
        let cached = CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1))
            .unwrap()
            .with_authentication_timeout(short);
        cached.inner.set_delay(short * 2);

        let err = cached
            .authenticate("tok")
            .await
            .expect_err("a backend slower than the overridden deadline must not resolve");
        assert!(matches!(err, InternalAuthNError::Unavailable));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_backend_times_out_rather_than_parking_every_caller() {
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();
        // Longer than the deadline, i.e. a backend that never answers.
        cached.inner.set_delay(DEFAULT_AUTHENTICATION_TIMEOUT * 2);

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
    async fn concurrent_callers_share_one_deadline_rather_than_queueing() {
        // The regression this guards: the deadline used to be taken *after* the
        // per-token lock, so three callers presenting the same token against a
        // hung backend each started their own timer on reaching the front of
        // the queue and finished at roughly 1x, 2x and 3x the deadline. All
        // three must now fail within one deadline of their own arrival.
        let cached = Arc::new(
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap(),
        );
        cached.inner.set_delay(DEFAULT_AUTHENTICATION_TIMEOUT * 10);

        // `tokio::time::Instant`, not `std::time::Instant`: under a paused
        // clock, tokio fast-forwards to the next timer with no real-time delay,
        // so `std::time::Instant::elapsed()` would read near-zero regardless of
        // how much virtual time passed -- the assertion below would pass even
        // with the old queueing bug.
        let started = tokio::time::Instant::now();
        let mut callers = Vec::new();
        for _ in 0..3 {
            let cached = Arc::clone(&cached);
            callers.push(tokio::spawn(async move {
                let outcome = cached.authenticate("same-token").await;
                (outcome, started.elapsed())
            }));
        }

        for caller in callers {
            let (outcome, elapsed) = caller.await.expect("task must not panic");
            assert!(
                matches!(outcome, Err(InternalAuthNError::Unavailable)),
                "a hung backend must surface as Unavailable, got {outcome:?}"
            );
            assert!(
                elapsed < DEFAULT_AUTHENTICATION_TIMEOUT * 2,
                "caller waited {elapsed:?}, i.e. it queued behind another \
                 caller's deadline instead of holding its own"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_very_long_token_obeys_the_deadline_too() {
        // Length no longer changes the path, but the deadline must still cover
        // a long credential like any other.
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();
        cached.inner.set_delay(DEFAULT_AUTHENTICATION_TIMEOUT * 2);

        let huge = "x".repeat(64 * 1024);
        let err = cached
            .authenticate(&huge)
            .await
            .expect_err("a long token must be bounded as well");
        assert!(matches!(err, InternalAuthNError::Unavailable));
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_validation_is_not_cached() {
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap();
        cached.inner.set_delay(DEFAULT_AUTHENTICATION_TIMEOUT * 2);
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
            cached.insert(TokenKey::of(&name), valid_entry(&name, expires_at), now);
        }
        assert_eq!(cached.cache.lock().len(), MAX_CACHE_ENTRIES);

        cached.insert(
            TokenKey::of("overflow"),
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
            cache.contains_key(&TokenKey::of("overflow")),
            "the newly inserted token must be present"
        );
        assert!(
            !cache.contains_key(&TokenKey::of("tok-0")),
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
            cached.insert(TokenKey::of(&name), valid_entry(&name, expires_at), now);
        }
        // Insert the already-expired entry last so a periodic sweep during the
        // fill loop above cannot reclaim it before the capacity path runs.
        cached.insert(
            TokenKey::of("expired"),
            valid_entry("expired", now.checked_sub(Duration::from_secs(1)).unwrap()),
            now,
        );
        assert_eq!(cached.cache.lock().len(), MAX_CACHE_ENTRIES);

        cached.insert(
            TokenKey::of("overflow"),
            valid_entry("overflow", now + Duration::from_mins(10)),
            now,
        );

        let cache = cached.cache.lock();
        assert_eq!(cache.len(), MAX_CACHE_ENTRIES);
        assert!(cache.contains_key(&TokenKey::of("overflow")));
        assert!(
            !cache.contains_key(&TokenKey::of("expired")),
            "the expired entry must be reclaimed by the sweep, making room"
        );
        assert!(
            cache.contains_key(&TokenKey::of("tok-1")),
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
        //
        // The lower bound is `now`, not `now + 1s`: `exp` is whole seconds, so
        // truncation leaves anywhere from just over 1s to a full 2s of life,
        // and a tighter bound would fail on scheduling delay alone.
        assert!(
            expiry > now,
            "a token with life left must produce a non-zero cache lifetime"
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
    async fn cancelling_a_waiter_still_releases_its_inflight_registration() {
        // Regression: `ReleaseTokenLockOnDrop` used to be constructed *after*
        // `lock.lock().await`. For 32 distinct tokens: A takes the lock and
        // enters a very slow backend call; B queues behind A on the same
        // token. Cancelling A first is fine -- its cleanup sees B still holds
        // a clone and correctly leaves the entry. But cancelling B *before it
        // resumes* used to leak: B had no guard yet, so nothing ever asked its
        // entry to remove itself, and it outlived every reference to it.
        let cached = Arc::new(
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1)).unwrap(),
        );
        cached.inner.set_delay(Duration::from_secs(30));

        for i in 0..32 {
            let token = format!("tok-{i}");

            let a = tokio::spawn({
                let cached = Arc::clone(&cached);
                let token = token.clone();
                async move { cached.authenticate(&token).await }
            });
            // Let A win the per-token lock and enter the (slow) backend call.
            tokio::time::sleep(Duration::from_millis(5)).await;

            let b = tokio::spawn({
                let cached = Arc::clone(&cached);
                let token = token.clone();
                async move { cached.authenticate(&token).await }
            });
            // Let B register for the same token and start waiting on the lock
            // A already holds.
            tokio::time::sleep(Duration::from_millis(5)).await;

            // Abort both back to back, with no `.await` between them: once a
            // task is marked aborted, tokio drops its future from whatever
            // state it was suspended in rather than polling it further, so B
            // is captured genuinely still waiting for the lock -- not given a
            // chance to run once A's release wakes it.
            a.abort();
            b.abort();
            drop(a.await);
            drop(b.await);
        }

        assert_eq!(
            cached.inflight.lock().len(),
            0,
            "every cancelled waiter -- holding the lock or still queued for it -- \
             must release its `inflight` registration"
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
