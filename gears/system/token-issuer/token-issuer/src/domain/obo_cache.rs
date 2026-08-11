//! OBO idempotency cache (DESIGN.md § 3.6).
//!
//! Per-process and keyed by `(cap jti, canonical scope set)`: re-minting on the
//! same replica with the same cap token and the same down-scoped grant returns
//! the byte-identical OBO token, so a retried adapter callback pinned to that
//! replica does not churn fresh tokens. An entry lives until
//! the cap's Gate-1 acceptance horizon (`cap_valid_until` = cap `exp` +
//! `clock_skew_secs`), not bare cap `exp`: Gate 1 still accepts the cap during
//! the skew window, so the cache must too, or a retry in that window would
//! break the byte-identical guarantee. If the cached OBO has expired but its
//! cap is still acceptable, the next re-mint replaces it in place.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{Mutex, RwLock};
use toolkit_macros::domain_model;
use uuid::Uuid;

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

/// Maximum number of due deadline records processed by one cleanup pass.
const CLEANUP_BATCH_SIZE: usize = 64;
/// Run bounded cleanup at least once per this many lookups, including hits.
const CLEANUP_INTERVAL: u64 = 16;

#[derive(Clone)]
struct DeadlineRecord {
    key: OboCacheKey,
    /// `None` indexes a newly installed empty slot; `Some` validates a value.
    expected_deadline: Option<i64>,
}

#[derive(Default)]
struct CacheState {
    slots: HashMap<OboCacheKey, Slot>,
    /// Records are keyed by retry time, separately from their real deadline.
    deadlines: BTreeMap<i64, Vec<DeadlineRecord>>,
}

/// Idempotency cache for OBO tokens.
///
/// Each key owns its own [`Mutex`] so the read-check-mint-insert sequence is
/// atomic per key: concurrent re-mints for the same `(cap_jti, scope_hash)`
/// serialize on that lock and the first mint's token is reused, preserving the
/// idempotency guarantee. Distinct keys can briefly contend while inserting
/// slots or performing bounded deadline cleanup, but signing is never performed
/// under the global state lock.
#[domain_model]
#[derive(Default)]
pub struct OboCache {
    state: RwLock<CacheState>,
    operations: AtomicU64,
}

impl OboCache {
    /// Creates an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Removes a bounded number of entries past their cap acceptance horizon.
    /// Records retain the value's real deadline while busy retries move to a
    /// later scheduling key, preventing one full busy batch from starving later
    /// due records. `None` records reclaim failed or cancelled pending slots.
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
                                cached.cap_valid_until == expected && cached.cap_valid_until <= now
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
    pub async fn get_or_mint<F, Fut, E>(
        &self,
        key: &OboCacheKey,
        cap_valid_until: i64,
        now: i64,
        mint: F,
    ) -> Result<String, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(String, i64), E>>,
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
            && c.cap_valid_until > now
            && c.obo_exp > now
        {
            return Ok(c.jwt.clone());
        }

        // Process only a fixed number of indexed deadlines, and release the
        // state lock before signing so distinct-key misses do not serialize.
        {
            let mut state = self.state.write().await;
            Self::cleanup_expired(&mut state, now);
        }
        let (jwt, obo_exp) = mint().await?;

        // Lock ordering is slot then state. Cleanup takes state but only uses
        // non-awaiting try_lock on slots, so it cannot deadlock this path. Once
        // the state lock is acquired, publication and final indexing are
        // synchronous: cancellation cannot expose an unindexed value.
        let mut state = self.state.write().await;
        *inner = Some(Cached {
            jwt: jwt.clone(),
            obo_exp,
            cap_valid_until,
        });
        state
            .deadlines
            .entry(cap_valid_until)
            .or_default()
            .push(DeadlineRecord {
                key: key.clone(),
                expected_deadline: Some(cap_valid_until),
            });
        Ok(jwt)
    }
}

#[cfg(test)]
#[path = "obo_cache_tests.rs"]
mod tests;
