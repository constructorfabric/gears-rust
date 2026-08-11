//! Get-or-mint cache for capability tokens.
//!
//! A cap token is reused while its remaining TTL exceeds the configured reuse
//! floor; otherwise the supplied `mint` closure produces a fresh one. Identical
//! caller contexts (same key) collapse onto one cached token.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};
use toolkit_macros::domain_model;
use uuid::Uuid;

use token_issuer_sdk::TokenIssuerError;

/// Identity of a cached capability token.
///
/// `scopes_hash` is `sha256` of the canonical scope string so the (variable
/// length) scope set folds into a fixed-size, hashable component.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// Subject (caller user id).
    pub sub: Uuid,
    /// Subject's home tenant.
    pub subject_tenant: Uuid,
    /// Tenant context the token is minted for.
    pub context_tenant: Uuid,
    /// Optional project context.
    pub context_project_id: Option<Uuid>,
    /// Audience.
    pub aud: String,
    /// SHA-256 of the canonical scope string.
    pub scopes_hash: [u8; 32],
    /// Operation baked into the signed claims.
    pub operation: Option<String>,
    /// Resource type baked into the signed claims.
    pub resource_type: Option<String>,
}

/// A cached token together with its expiry.
#[domain_model]
#[derive(Clone)]
struct Cached {
    jwt: String,
    exp: i64,
}

/// Per-key slot guarding the read-check-mint-insert sequence (empty until first mint).
type Slot = Arc<Mutex<Option<Cached>>>;

/// Whether `get_or_mint` served a cached token or minted a fresh one.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheOutcome {
    /// A still-fresh cached token was reused.
    Hit,
    /// No fresh token was cached; `mint` was invoked.
    Miss,
}

/// Capability-token cache with a reuse floor.
///
/// Each key owns its own [`Mutex`] so the read-check-mint-insert sequence is
/// atomic per key: concurrent mints for the same caller context serialize on
/// that lock and collapse onto one Transit sign. Distinct keys never contend.
/// Expired entries are pruned on each call so the map stays bounded.
#[domain_model]
pub struct CapCache {
    floor_secs: i64,
    map: RwLock<HashMap<CacheKey, Slot>>,
}

impl CapCache {
    /// Creates an empty cache that reuses tokens while remaining TTL exceeds
    /// `floor_secs`.
    #[must_use]
    pub fn new(floor_secs: u64) -> Self {
        Self {
            floor_secs: i64::try_from(floor_secs).unwrap_or(i64::MAX),
            map: RwLock::new(HashMap::new()),
        }
    }

    /// Returns a cached JWT while its remaining TTL exceeds the floor; otherwise
    /// awaits `mint`, caches the result, and returns it. The [`CacheOutcome`]
    /// tells the caller whether the token was reused or freshly minted (so it
    /// can record the right hit/miss metric).
    ///
    /// # Errors
    /// Propagates any error returned by `mint`.
    pub async fn get_or_mint<F, Fut>(
        &self,
        key: &CacheKey,
        now: i64,
        mint: F,
    ) -> Result<(String, CacheOutcome), TokenIssuerError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(String, i64), TokenIssuerError>>,
    {
        let slot = {
            let mut map = self.map.write().await;
            // Bounded growth: drop fully-expired entries. Skip slots currently
            // locked (in-flight mint or live reader).
            map.retain(|_, v| {
                v.try_lock()
                    .map_or(true, |inner| inner.as_ref().is_none_or(|c| c.exp > now))
            });
            Arc::clone(map.entry(key.clone()).or_default())
        };

        // Per-key lock: the read-check-mint-insert below is atomic for this key.
        let mut inner = slot.lock().await;
        if let Some(c) = inner.as_ref()
            && c.exp - now > self.floor_secs
        {
            return Ok((c.jwt.clone(), CacheOutcome::Hit));
        }
        let (jwt, exp) = mint().await?;
        *inner = Some(Cached {
            jwt: jwt.clone(),
            exp,
        });
        Ok((jwt, CacheOutcome::Miss))
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
