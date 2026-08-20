//! Short-lived positive caching for platform-plane authentication.
//!
//! [`CachingInternalAuthenticator`] wraps any
//! [`InternalAuthenticator`] with an in-memory, TTL-bounded cache of
//! **successful** validations. It exists because the Kubernetes `TokenReview`
//! backend ([`K8sTokenReviewAuthenticator`](crate::K8sTokenReviewAuthenticator))
//! performs a live API-server round-trip on every call — untenable on a hot
//! gRPC path where the same projected `ServiceAccount` token is presented on
//! back-to-back requests (`cpt-cf-adr-platform-plane-auth`, decision 5).
//!
//! # Semantics
//!
//! - Only `Ok` results are cached. Rejections
//!   ([`InternalAuthNError::InvalidToken`]) and backend failures
//!   ([`InternalAuthNError::Unavailable`]) are **never** cached, so a transient
//!   outage or a token that later becomes valid is re-evaluated on the next
//!   call rather than pinned to a stale answer.
//! - The cache key is the token itself, matched exactly. Exact matching avoids
//!   the collision risk of a non-cryptographic hash (two distinct tokens
//!   resolving to one cached identity) without pulling in a cryptographic
//!   digest — the workspace's validated crypto provider is platform-dependent,
//!   so this crate stays free of any direct crypto dependency. The token is
//!   already resident in process memory (request headers, the outbound
//!   credential), and entries are in-process and TTL-bounded.
//! - The TTL bounds the revocation window: a token revoked out-of-band is still
//!   accepted for at most `ttl` after its last successful validation. Keep it
//!   short (seconds) so the window stays small while still collapsing bursts.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use toolkit_security::{InternalAuthNError, InternalAuthenticator, PlatformIdentity};

/// Default time-to-live for a cached successful validation.
///
/// A conservative few seconds: long enough to collapse a burst of calls
/// carrying the same token, short enough to keep the post-revocation acceptance
/// window small.
pub const DEFAULT_TOKEN_REVIEW_CACHE_TTL: Duration = Duration::from_secs(30);

/// A cached successful validation and the instant it stops being valid.
struct CacheEntry {
    identity: PlatformIdentity,
    expires_at: Instant,
}

/// Wraps an [`InternalAuthenticator`] with a short-lived positive cache.
///
/// Construct it around the concrete validator and hand the wrapper to the
/// transport layer as the `InternalAuthenticator`:
///
/// ```rust,no_run
/// use std::time::Duration;
/// use toolkit_k8s_auth::{CachingInternalAuthenticator, K8sTokenReviewAuthenticator};
///
/// # async fn wire() -> Result<(), Box<dyn std::error::Error>> {
/// let validator =
///     K8sTokenReviewAuthenticator::try_default(vec!["toolkit-internal".to_owned()]).await?;
/// let cached = CachingInternalAuthenticator::new(validator, Duration::from_secs(30));
/// # let _ = cached;
/// # Ok(())
/// # }
/// ```
pub struct CachingInternalAuthenticator<A> {
    inner: A,
    ttl: Duration,
    cache: Mutex<HashMap<String, CacheEntry>>,
}

impl<A> std::fmt::Debug for CachingInternalAuthenticator<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachingInternalAuthenticator")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl<A> CachingInternalAuthenticator<A> {
    /// Wrap `inner`, caching successful validations for `ttl`.
    #[must_use]
    pub fn new(inner: A, ttl: Duration) -> Self {
        Self {
            inner,
            ttl,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Wrap `inner` with the [`DEFAULT_TOKEN_REVIEW_CACHE_TTL`].
    #[must_use]
    pub fn with_default_ttl(inner: A) -> Self {
        Self::new(inner, DEFAULT_TOKEN_REVIEW_CACHE_TTL)
    }

    /// Look up a still-valid cached identity for `token`.
    ///
    /// Holds the lock only for the duration of the map access (never across an
    /// `await`), so the returned future stays `Send`.
    fn lookup(&self, token: &str, now: Instant) -> Option<PlatformIdentity> {
        let cache = self.cache.lock();
        let entry = cache.get(token)?;
        (entry.expires_at > now).then(|| entry.identity.clone())
    }

    /// Store a freshly validated `identity` under `token` and drop expired entries.
    fn store(&self, token: String, identity: PlatformIdentity, now: Instant) {
        let expires_at = now + self.ttl;
        let mut cache = self.cache.lock();
        cache.retain(|_, entry| entry.expires_at > now);
        cache.insert(
            token,
            CacheEntry {
                identity,
                expires_at,
            },
        );
    }
}

impl<A: InternalAuthenticator> InternalAuthenticator for CachingInternalAuthenticator<A> {
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        let now = Instant::now();

        if let Some(identity) = self.lookup(token, now) {
            return Ok(identity);
        }

        // Cache miss: validate against the backend. Only cache success — a
        // rejection or backend outage must be re-evaluated next time.
        let identity = self.inner.authenticate(token).await?;
        self.store(token.to_owned(), identity.clone(), now);
        Ok(identity)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts backend calls and can be flipped between success and failure so
    /// tests can assert exactly when the wrapped authenticator is consulted.
    struct CountingAuth {
        calls: AtomicUsize,
        fail: AtomicUsize,
    }

    impl CountingAuth {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail: AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl InternalAuthenticator for CountingAuth {
        async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) > 0 {
                return Err(InternalAuthNError::Unavailable);
            }
            Ok(PlatformIdentity::Shared {
                name: token.to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn second_call_within_ttl_hits_cache() {
        let cached = CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1));

        let a = cached.authenticate("tok").await.unwrap();
        let b = cached.authenticate("tok").await.unwrap();
        assert_eq!(a, b);
        assert_eq!(
            cached.inner.calls(),
            1,
            "second call must be served from cache"
        );
    }

    #[tokio::test]
    async fn distinct_tokens_are_cached_independently() {
        let cached = CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1));

        cached.authenticate("a").await.unwrap();
        cached.authenticate("b").await.unwrap();
        cached.authenticate("a").await.unwrap();
        assert_eq!(
            cached.inner.calls(),
            2,
            "each distinct token validated once"
        );
    }

    #[tokio::test]
    async fn entry_expires_after_ttl() {
        let cached =
            CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_millis(20));

        cached.authenticate("tok").await.unwrap();
        assert_eq!(cached.inner.calls(), 1);

        tokio::time::sleep(Duration::from_millis(40)).await;

        cached.authenticate("tok").await.unwrap();
        assert_eq!(
            cached.inner.calls(),
            2,
            "expired entry must be re-validated"
        );
    }

    #[tokio::test]
    async fn errors_are_not_cached() {
        let cached = CachingInternalAuthenticator::new(CountingAuth::new(), Duration::from_mins(1));
        cached.inner.fail.store(1, Ordering::SeqCst);

        assert!(cached.authenticate("tok").await.is_err());
        assert!(cached.authenticate("tok").await.is_err());
        assert_eq!(cached.inner.calls(), 2, "failures must not be cached");

        // Once the backend recovers, the next call succeeds and is then cached.
        cached.inner.fail.store(0, Ordering::SeqCst);
        cached.authenticate("tok").await.unwrap();
        cached.authenticate("tok").await.unwrap();
        assert_eq!(
            cached.inner.calls(),
            3,
            "recovery validated once, then cached"
        );
    }
}
