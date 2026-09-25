//! The in-process replay cache.
//!
//! The hottest idempotency keys are retried within seconds of the original
//! call, usually by the same replica, so a small bounded cache answers most
//! replays without a database round trip. It is an optimization and never an
//! authority: a miss falls through to storage, which owns the record.
//!
//! An entry never outlives the record behind it. The database treats a key as
//! new once its retention window has elapsed, so a replica that kept answering
//! from memory past that instant would replay an operation the database would
//! have re-evaluated. Every entry therefore expires at the earlier of the
//! configured cache lifetime and the record's own deadline.

use std::collections::HashMap;

use parking_lot::Mutex;
use quota_enforcement_sdk::{Decision, IdempotencyRecord, IdempotencyScope, PayloadHash};
use time::OffsetDateTime;
use toolkit_macros::domain_model;

/// What a cached record answers with.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayRecord {
    /// Digest the replay's payload must match.
    pub payload_hash: PayloadHash,
    /// The decision the original operation produced.
    pub decision: Decision,
    /// Retention deadline of the stored record.
    pub expires_at: OffsetDateTime,
}

impl TryFrom<IdempotencyRecord> for ReplayRecord {
    type Error = serde_json::Error;

    fn try_from(record: IdempotencyRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            payload_hash: record.payload_hash,
            decision: record.decision()?,
            expires_at: record.expires_at,
        })
    }
}

struct Entry {
    record: ReplayRecord,
    deadline: OffsetDateTime,
    last_used: u64,
}

struct Cache {
    capacity: usize,
    tick: u64,
    entries: HashMap<IdempotencyScope, Entry>,
}

/// Bounded, least-recently-used cache of recent replay records.
pub struct IdempotencyCache {
    cache: Mutex<Cache>,
    ttl: time::Duration,
}

impl IdempotencyCache {
    /// A cache of at most `capacity` entries, each living at most `ttl`.
    #[must_use]
    pub fn new(capacity: usize, ttl: std::time::Duration) -> Self {
        Self {
            cache: Mutex::new(Cache {
                capacity: capacity.max(1),
                tick: 0,
                entries: HashMap::new(),
            }),
            ttl: time::Duration::try_from(ttl).unwrap_or(time::Duration::ZERO),
        }
    }

    /// The record cached under `scope`, if one is still live at `now`.
    ///
    /// An entry past its deadline is dropped rather than returned: the
    /// database may already treat its key as a new operation.
    #[must_use]
    pub fn get(&self, scope: &IdempotencyScope, now: OffsetDateTime) -> Option<ReplayRecord> {
        let mut cache = self.cache.lock();
        cache.tick += 1;
        let tick = cache.tick;
        let entry = cache.entries.get_mut(scope)?;
        if entry.deadline <= now {
            cache.entries.remove(scope);
            return None;
        }
        entry.last_used = tick;
        Some(entry.record.clone())
    }

    /// Cache what storage answered, capped by the record's own retention.
    pub fn insert(&self, scope: IdempotencyScope, record: ReplayRecord, now: OffsetDateTime) {
        let deadline = (now + self.ttl).min(record.expires_at);
        if deadline <= now {
            // The record is already at its deadline; caching it would only
            // make a stale answer possible.
            return;
        }
        let mut cache = self.cache.lock();
        cache.tick += 1;
        let tick = cache.tick;
        // Evict the least recently used entry. Linear in the capacity, which
        // is small and configured; a heap would cost more than it saves.
        if !cache.entries.contains_key(&scope)
            && cache.entries.len() >= cache.capacity
            && let Some(victim) = cache
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
        {
            cache.entries.remove(&victim);
        }
        cache.entries.insert(
            scope,
            Entry {
                record,
                deadline,
                last_used: tick,
            },
        );
    }

    /// Number of live entries. For tests and for an operator gauge later.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.lock().entries.len()
    }

    /// Whether the cache holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "idempotency_tests.rs"]
mod tests;
