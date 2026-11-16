//! A short-lived memo of consume-path PDP answers, so one question asked
//! twice within its TTL (a JOIN, then the stream open that follows it) costs
//! one PDP call.
//!
//! The key is the request the PDP receives, so two calls share an answer only
//! when they send the same thing. Time is passed in by the caller: the memo
//! never reads a clock and never awaits.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use authz_resolver_sdk::EvaluationRequest;
use tokio::time::Instant;
use toolkit::domain_model;

use crate::domain::streaming::scope::CompiledScope;

/// PDP answers by the exact request that produced them, each until its
/// expiry.
///
/// At most [`Self::CAPACITY`] entries. A full memo drops its expired entries
/// before it takes a new key; if it is still full, the new key is not stored.
/// An existing key is always replaced.
#[domain_model]
#[derive(Debug, Default)]
pub struct ScopeMemo {
    entries: Mutex<HashMap<MemoKey, MemoEntry>>,
}

/// The request a PDP call sends, as the PDP receives it.
///
/// It holds the serialized `EvaluationRequest`: subject, action, resource with
/// its properties, the full tenant context, token scopes,
/// `require_constraints`, capabilities and supported properties. Two keys are
/// equal only when the requests are equal, whatever the order of their
/// property maps.
///
/// The bearer token is not serialized, so it is outside the key: a memoized
/// answer outlives the caller's token for at most its TTL. Tying a cached
/// decision to the token is left to the `AuthZ` protocol (#5502).
#[domain_model]
#[derive(Debug, Clone)]
pub struct MemoKey {
    fingerprint: u64,
    request: Arc<serde_json::Value>,
}

/// One answer and the instant it stops serving.
#[derive(Debug)]
struct MemoEntry {
    result: MemoResult,
    expires_at: Instant,
}

/// A memoized PDP answer.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum MemoResult {
    /// A compiled consume scope, shared rather than copied.
    Scope(Arc<CompiledScope>),
    /// A yes or no decision; a stored `false` is returned like any other.
    Decision(bool),
}

/// What [`ScopeMemo::insert`] did.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum MemoInsert {
    /// The answer is stored, as a new key or replacing the old answer.
    Stored,
    /// The memo holds [`ScopeMemo::CAPACITY`] live entries; nothing is stored.
    Full,
}

/// A PDP request that could not be turned into a memo key.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a PDP request could not be serialized into a memo key")]
pub struct MemoKeyError;

impl ScopeMemo {
    /// The most entries the memo holds. A new key that finds it full is
    /// stored only if dropping the expired entries makes room.
    pub const CAPACITY: usize = 1_024;

    /// An empty memo.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The answer stored for `key` while `now` is before its expiry.
    ///
    /// An expired entry is removed and reads as `None`.
    #[must_use]
    pub fn get(&self, key: &MemoKey, now: Instant) -> Option<MemoResult> {
        self.get_entry(key, now).map(|(result, _)| result)
    }

    /// The answer stored for `key` and the instant it stops serving, while
    /// `now` is before that instant.
    ///
    /// An expired entry is removed and reads as `None`.
    #[must_use]
    pub fn get_entry(&self, key: &MemoKey, now: Instant) -> Option<(MemoResult, Instant)> {
        let mut entries = self.entries();
        let entry = entries.get(key)?;
        if entry.is_live(now) {
            return Some((entry.result.clone(), entry.expires_at));
        }
        entries.remove(key);
        None
    }

    /// Stores `result` for `key` until `expires_at`.
    ///
    /// A new key in a full memo first drops every entry expired at `now`, and
    /// is refused with [`MemoInsert::Full`] if the memo is still full.
    pub fn insert(
        &self,
        key: MemoKey,
        result: MemoResult,
        expires_at: Instant,
        now: Instant,
    ) -> MemoInsert {
        let mut entries = self.entries();
        if entries.len() >= Self::CAPACITY && !entries.contains_key(&key) {
            entries.retain(|_, entry| entry.is_live(now));
            if entries.len() >= Self::CAPACITY {
                return MemoInsert::Full;
            }
        }
        entries.insert(key, MemoEntry { result, expires_at });
        MemoInsert::Stored
    }

    /// Recovers a poisoned guard rather than failing every later call.
    ///
    /// Each critical section is one lookup and remove, one `retain` or one
    /// insert, so a panic inside one leaves a valid map. The poison is sticky:
    /// treating it as a failure would fail every later lookup for the life of
    /// the process.
    fn entries(&self) -> MutexGuard<'_, HashMap<MemoKey, MemoEntry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl MemoEntry {
    fn is_live(&self, now: Instant) -> bool {
        now < self.expires_at
    }
}

impl MemoKey {
    /// A key with a chosen fingerprint, so a test can put two requests under
    /// one.
    #[cfg(test)]
    fn with_fingerprint(fingerprint: u64, request: serde_json::Value) -> Self {
        Self {
            fingerprint,
            request: Arc::new(request),
        }
    }
}

impl TryFrom<&EvaluationRequest> for MemoKey {
    type Error = MemoKeyError;

    /// # Errors
    ///
    /// [`MemoKeyError`] only on a broken internal invariant: serializing an
    /// `EvaluationRequest` fails only on a map with non-string keys, which its
    /// types rule out. The error carries nothing of the request.
    fn try_from(request: &EvaluationRequest) -> Result<Self, Self::Error> {
        let request = serde_json::to_value(request).map_err(|_| MemoKeyError)?;
        // `Value`'s own `Hash` walks object entries in key order in either
        // map mode, so the order of the request's property maps never
        // changes the fingerprint.
        let mut hasher = DefaultHasher::new();
        request.hash(&mut hasher);
        Ok(Self {
            fingerprint: hasher.finish(),
            request: Arc::new(request),
        })
    }
}

/// The fingerprint only: equal keys have equal fingerprints, and `Eq` settles
/// a collision.
impl Hash for MemoKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.fingerprint.hash(state);
    }
}

impl PartialEq for MemoKey {
    fn eq(&self, other: &Self) -> bool {
        self.fingerprint == other.fingerprint && self.request == other.request
    }
}

impl Eq for MemoKey {}

#[cfg(test)]
#[path = "scope_memo_tests.rs"]
mod scope_memo_tests;
