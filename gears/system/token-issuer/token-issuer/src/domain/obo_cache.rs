//! OBO idempotency cache (DESIGN.md § 3.6).
//!
//! Keyed by `(cap jti, canonical scope set)`: re-minting with the same cap token
//! and the same down-scoped grant returns the byte-identical OBO token, so a
//! retried adapter callback does not churn fresh tokens. An entry lives until
//! the cap's Gate-1 acceptance horizon (`cap_valid_until` = cap `exp` +
//! `clock_skew_secs`), not bare cap `exp`: Gate 1 still accepts the cap during
//! the skew window, so the cache must too, or a retry in that window would
//! break the byte-identical guarantee. If the cached OBO has expired but its
//! cap is still acceptable, the next re-mint replaces it in place.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};
use toolkit_macros::domain_model;
use uuid::Uuid;

use token_issuer_sdk::TokenIssuerError;

/// Identity of a cached OBO token: the cap token's `jti` plus the SHA-256 of the
/// canonical down-scoped scope set (so different grants from the same cap
/// coexist as distinct entries).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OboCacheKey {
    /// Originating capability token id.
    pub cap_jti: Uuid,
    /// SHA-256 of the canonical down-scoped scope string.
    pub scopes_hash: [u8; 32],
}

impl OboCacheKey {
    /// Builds a key from the cap `jti` and a scope hash.
    #[must_use]
    pub fn new(cap_jti: Uuid, scopes_hash: [u8; 32]) -> Self {
        Self {
            cap_jti,
            scopes_hash,
        }
    }
}

/// A cached OBO token with its own expiry and its cap's Gate-1 acceptance
/// horizon (cap `exp` + `clock_skew_secs`).
#[domain_model]
#[derive(Clone)]
struct Cached {
    jwt: String,
    obo_exp: i64,
    cap_valid_until: i64,
}

/// Per-key slot guarding the read-check-mint-insert sequence (empty until first mint).
type Slot = Arc<Mutex<Option<Cached>>>;

/// Idempotency cache for OBO tokens.
///
/// Each key owns its own [`Mutex`] so the read-check-mint-insert sequence is
/// atomic per key: concurrent re-mints for the same `(cap_jti, scope_hash)`
/// serialize on that lock and the first mint's token is reused, preserving the
/// idempotency guarantee. Distinct keys never contend. Cap-expired entries are
/// pruned on each call so the map stays bounded under steady traffic.
#[domain_model]
#[derive(Default)]
pub struct OboCache {
    map: RwLock<HashMap<OboCacheKey, Slot>>,
}

impl OboCache {
    /// Creates an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached OBO token for `key` while it is still valid; otherwise
    /// awaits `mint`, stores the result (replacing any stale entry in place), and
    /// returns it.
    ///
    /// A cached entry is reused only when its OBO has not expired (`obo_exp >
    /// now`). Entries past their cap's Gate-1 acceptance horizon
    /// (`cap_valid_until <= now`) are evicted. `cap_valid_until` MUST be the
    /// cap `exp` plus the same `clock_skew_secs` Gate 1 accepts with, so the
    /// idempotency window matches the re-mint acceptance window. `mint` yields
    /// `(jwt, obo_exp)`.
    ///
    /// # Errors
    /// Propagates any error returned by `mint`.
    pub async fn get_or_mint<F, Fut>(
        &self,
        key: &OboCacheKey,
        cap_valid_until: i64,
        now: i64,
        mint: F,
    ) -> Result<String, TokenIssuerError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(String, i64), TokenIssuerError>>,
    {
        let slot = {
            let mut map = self.map.write().await;
            // Bounded growth: drop entries past their cap's Gate-1 acceptance
            // horizon. Skip slots currently locked (in-flight mint or live reader).
            map.retain(|_, v| {
                v.try_lock().map_or(true, |inner| {
                    inner.as_ref().is_none_or(|c| c.cap_valid_until > now)
                })
            });
            Arc::clone(map.entry(key.clone()).or_default())
        };

        // Per-key lock: the read-check-mint-insert below is atomic for this key.
        let mut inner = slot.lock().await;
        if let Some(c) = inner.as_ref()
            && c.cap_valid_until > now
            && c.obo_exp > now
        {
            return Ok(c.jwt.clone());
        }
        let (jwt, obo_exp) = mint().await?;
        *inner = Some(Cached {
            jwt: jwt.clone(),
            obo_exp,
            cap_valid_until,
        });
        Ok(jwt)
    }
}

#[cfg(test)]
#[path = "obo_cache_tests.rs"]
mod tests;
