//! Get-or-mint cache for capability tokens.
//!
//! A cap token is reused while its remaining TTL exceeds the configured reuse
//! floor; otherwise the supplied `mint` closure produces a fresh one. Identical
//! caller contexts (same key) collapse onto one cached token.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Maximum number of due deadline records processed by one cleanup pass.
const CLEANUP_BATCH_SIZE: usize = 64;
/// Run bounded cleanup at least once per this many lookups, including hits.
const CLEANUP_INTERVAL: u64 = 16;

#[derive(Clone)]
struct DeadlineRecord {
    key: CacheKey,
    /// `None` indexes a newly installed empty slot; `Some` validates a value.
    expected_deadline: Option<i64>,
}

#[derive(Default)]
struct CacheState {
    slots: HashMap<CacheKey, Slot>,
    /// Records are keyed by retry time, separately from their real deadline.
    deadlines: BTreeMap<i64, Vec<DeadlineRecord>>,
}

/// Capability-token cache with a reuse floor.
///
/// Each key owns its own [`Mutex`] so the read-check-mint-insert sequence is
/// atomic per key: concurrent mints for the same caller context serialize on
/// that lock and collapse onto one Transit sign. Distinct keys can briefly
/// contend while inserting slots or performing bounded deadline cleanup, but
/// signing is never performed under the global state lock.
#[domain_model]
pub struct CapCache {
    floor_secs: i64,
    state: RwLock<CacheState>,
    operations: AtomicU64,
}

impl CapCache {
    /// Creates an empty cache that reuses tokens while remaining TTL exceeds
    /// `floor_secs`.
    #[must_use]
    pub fn new(floor_secs: u64) -> Self {
        Self {
            floor_secs: i64::try_from(floor_secs).unwrap_or(i64::MAX),
            state: RwLock::new(CacheState::default()),
            operations: AtomicU64::new(0),
        }
    }

    /// Removes a bounded number of due entries. Records retain the value's real
    /// deadline while busy retries move to a later scheduling key, preventing a
    /// full busy batch from starving later due records. `None` records reclaim
    /// empty slots left by mint errors or cancellation.
    fn cleanup_expired(state: &mut CacheState, now: i64) {
        enum Action {
            Remove,
            Retry,
            Discard,
        }

        let retry_at = now.saturating_add(1);
        for _ in 0..CLEANUP_BATCH_SIZE {
            let Some(mut entry) = state.deadlines.first_entry() else {
                break;
            };
            if *entry.key() > now {
                break;
            }
            let Some(record) = entry.get_mut().pop() else {
                entry.remove();
                continue;
            };
            if entry.get().is_empty() {
                entry.remove();
            }

            let action = match state.slots.get(&record.key) {
                None => Action::Discard,
                Some(slot) if Arc::strong_count(slot) > 1 => Action::Retry,
                Some(slot) => match slot.try_lock() {
                    Err(_) => Action::Retry,
                    Ok(inner) => match record.expected_deadline {
                        None if inner.is_none() => Action::Remove,
                        Some(expected)
                            if inner.as_ref().is_some_and(|cached| {
                                cached.exp == expected && cached.exp <= now
                            }) =>
                        {
                            Action::Remove
                        }
                        _ => Action::Discard,
                    },
                },
            };

            match action {
                Action::Remove => {
                    state.slots.remove(&record.key);
                }
                Action::Retry => state.deadlines.entry(retry_at).or_default().push(record),
                Action::Discard => {}
            }
        }
    }

    async fn cleanup_if_due(&self, now: i64) {
        let operation = self.operations.fetch_add(1, Ordering::Relaxed);
        if operation.wrapping_add(1).is_multiple_of(CLEANUP_INTERVAL) {
            let mut state = self.state.write().await;
            Self::cleanup_expired(&mut state, now);
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
        // Amortized cleanup runs before acquiring a slot, so hit-only traffic
        // also drives bounded reclamation without ever scanning the slot map.
        self.cleanup_if_due(now).await;

        // Existing-key lookups clone the slot under a read lock. Only a missing
        // key needs the global write lock to atomically install both the empty
        // slot and its provisional cleanup record.
        let existing = {
            let state = self.state.read().await;
            state.slots.get(key).cloned()
        };
        let slot = if let Some(slot) = existing {
            slot
        } else {
            let mut state = self.state.write().await;
            if let Some(slot) = state.slots.get(key) {
                Arc::clone(slot)
            } else {
                let slot = Arc::new(Mutex::new(None));
                state.slots.insert(key.clone(), Arc::clone(&slot));
                state
                    .deadlines
                    .entry(now)
                    .or_default()
                    .push(DeadlineRecord {
                        key: key.clone(),
                        expected_deadline: None,
                    });
                slot
            }
        };

        // Per-key lock: the read-check-mint-insert below is atomic for this key.
        let mut inner = slot.lock().await;
        if let Some(c) = inner.as_ref()
            && c.exp - now > self.floor_secs
        {
            return Ok((c.jwt.clone(), CacheOutcome::Hit));
        }

        // Bound growth without scanning the slot map. This lock is released
        // before signing, so misses for distinct keys do not serialize mints.
        {
            let mut state = self.state.write().await;
            Self::cleanup_expired(&mut state, now);
        }
        let (jwt, exp) = mint().await?;

        // Lock ordering is slot then state. Cleanup takes state but only uses
        // non-awaiting try_lock on slots, so it cannot deadlock this path. Once
        // the state lock is acquired, publication and final indexing are
        // synchronous: cancellation cannot expose an unindexed value.
        let mut state = self.state.write().await;
        *inner = Some(Cached {
            jwt: jwt.clone(),
            exp,
        });
        state
            .deadlines
            .entry(exp)
            .or_default()
            .push(DeadlineRecord {
                key: key.clone(),
                expected_deadline: Some(exp),
            });
        Ok((jwt, CacheOutcome::Miss))
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
