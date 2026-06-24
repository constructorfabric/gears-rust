// Created: 2026-06-24 by Constructor Tech
//! Property / model-based variant for the cache contract (§4 "Model-based /
//! property variants").
//!
//! A random sequence of cache operations is replayed against both the backend
//! under test and a trivial reference model, asserting — after *every* op — the
//! invariants the contract guarantees under any interleaving the single-threaded
//! sequence permits. Where example-based [`cache`](crate::cache) scenarios pin
//! specific behaviors, this catches the CAS/version races a fixed example never
//! reaches.
//!
//! # What the oracle may and may not assert
//!
//! The reference model tracks **value and presence** exactly — those *are*
//! contract-guaranteed (`get` returns the last write; `delete` removes it). It
//! deliberately does **not** track an absolute version, because version
//! numbering is backend-specific: an in-memory backend mints `1` per key and
//! increments by one, but etcd (`mod_revision`) and NATS (stream revision) use a
//! store-wide counter that jumps. Asserting exact version equality would fail a
//! conformant revision-based backend. The contract guarantees only:
//!
//! - a present entry's version is **≠ 0** (0 is the reserved sentinel);
//! - each *successful mutation* of a continuously-present key **strictly
//!   increases** its version (a `delete` clears the watermark, so a re-create may
//!   reset lower — SC-CACHE-009);
//! - a *non-mutating* op (a conflicted CAS, a no-op delete) leaves the version
//!   **unchanged** — so a backend that bumps a version on a failed CAS is caught;
//! - `compare_and_swap` succeeds **iff** `expected_version` equals the backend's
//!   *own* current version — which is why [`VersionTarget`] is resolved against a
//!   live read at replay time, not baked into the generated op.
//!
//! The `proptest` strategy that generates the op sequences lives in the crate's
//! `tests/model.rs` (proptest is a dev-dependency); this module is the
//! runtime-agnostic replay engine those tests drive.

use std::collections::HashMap;
use std::sync::Arc;

use cluster_sdk::cache::{ClusterCacheBackend, PutRequest, Ttl};
use cluster_sdk::error::ClusterError;

/// A trivial single-threaded reference cache: value + presence only, the part of
/// the contract that holds identically across every backend. Version invariants
/// are checked by [`replay_against_model`] from the backend's own readings, not
/// stored here (see the module docs).
#[derive(Debug, Default)]
pub struct CacheModel {
    map: HashMap<String, Vec<u8>>,
}

impl CacheModel {
    /// The value currently stored under `key`, if any.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Vec<u8>> {
        self.map.get(key)
    }

    /// Whether `key` is present.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// Unconditional overwrite.
    pub fn put(&mut self, key: &str, value: &[u8]) {
        self.map.insert(key.to_owned(), value.to_vec());
    }

    /// Create-if-absent; returns whether it created.
    pub fn create_if_absent(&mut self, key: &str, value: &[u8]) -> bool {
        if self.map.contains_key(key) {
            false
        } else {
            self.map.insert(key.to_owned(), value.to_vec());
            true
        }
    }

    /// Delete; returns whether the key existed.
    pub fn remove(&mut self, key: &str) -> bool {
        self.map.remove(key).is_some()
    }
}

/// How a generated [`CacheOp::CompareAndSwap`] picks its `expected_version`. The
/// concrete version is resolved against a live read at replay time so the
/// success path fires regardless of a backend's version numbering scheme.
#[derive(Debug, Clone, Copy)]
pub enum VersionTarget {
    /// Use the key's current backend version — CAS should succeed iff present.
    Current,
    /// Use a deliberately-stale version — CAS should always conflict.
    Stale,
}

/// One operation in the model alphabet. A `proptest` strategy (in `tests/`)
/// generates `Vec<CacheOp>` sequences over a small key/value space so create /
/// CAS / delete collisions are frequent.
#[derive(Debug, Clone)]
pub enum CacheOp {
    /// Unconditional overwrite (always mutates; version strictly increases).
    Put { key: String, value: Vec<u8> },
    /// Atomic create-if-absent (mutates iff it created).
    PutIfAbsent { key: String, value: Vec<u8> },
    /// Version-guarded compare-and-swap (mutates iff it succeeded).
    CompareAndSwap {
        key: String,
        target: VersionTarget,
        value: Vec<u8>,
    },
    /// Unconditional delete (mutates iff the key existed).
    Delete { key: String },
}

impl CacheOp {
    /// The key this op targets.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Put { key, .. }
            | Self::PutIfAbsent { key, .. }
            | Self::CompareAndSwap { key, .. }
            | Self::Delete { key } => key,
        }
    }
}

/// Per-key high-water version mark, used to enforce strict monotonicity across
/// consecutive present states and equality across non-mutating ops.
type Watermarks = HashMap<String, u64>;

/// Replays `ops` against a fresh backend from `make`, asserting the contract's
/// value/presence and version invariants against the reference model after every
/// op. Returns `Err(reason)` on the first divergence so `proptest` can shrink to
/// a minimal failing sequence.
///
/// # Errors
/// Returns a human-readable description of the first invariant violation (or an
/// unexpected backend error), tagged with the op index and key.
pub async fn replay_against_model<F>(ops: &[CacheOp], make: F) -> Result<(), String>
where
    F: Fn() -> Arc<dyn ClusterCacheBackend>,
{
    let backend = make();
    let mut model = CacheModel::default();
    let mut marks: Watermarks = HashMap::new();

    for (i, op) in ops.iter().enumerate() {
        let mutated = apply_op(backend.as_ref(), &mut model, op, i).await?;
        check_after(backend.as_ref(), &model, &mut marks, op, i, mutated).await?;
    }
    Ok(())
}

/// Applies `op` to both the backend and the model, asserting the success/failure
/// of conditional ops agrees with the model. Returns whether the op mutated the
/// key (drives the post-op version check).
async fn apply_op(
    backend: &dyn ClusterCacheBackend,
    model: &mut CacheModel,
    op: &CacheOp,
    i: usize,
) -> Result<bool, String> {
    match op {
        CacheOp::Put { key, value } => {
            backend
                .put(PutRequest {
                    key,
                    value,
                    ttl: Ttl::Indefinite,
                })
                .await
                .map_err(|e| fail(i, key, &format!("put errored: {e:?}")))?;
            model.put(key, value);
            Ok(true)
        }
        CacheOp::PutIfAbsent { key, value } => {
            let created = backend
                .put_if_absent(PutRequest {
                    key,
                    value,
                    ttl: Ttl::Indefinite,
                })
                .await
                .map_err(|e| fail(i, key, &format!("put_if_absent errored: {e:?}")))?
                .is_some();
            let model_created = model.create_if_absent(key, value);
            if created != model_created {
                return Err(fail(
                    i,
                    key,
                    &format!("put_if_absent created={created} but model expected {model_created}"),
                ));
            }
            Ok(created)
        }
        CacheOp::CompareAndSwap { key, target, value } => {
            let current = backend
                .get(key)
                .await
                .map_err(|e| fail(i, key, &format!("get (for CAS) errored: {e:?}")))?
                .map_or(0, |entry| entry.version);
            let expected = match target {
                VersionTarget::Current => current,
                // Distinct from the live version, so a present key always
                // mismatches and an absent key (current 0) conflicts anyway.
                VersionTarget::Stale => current.wrapping_add(1),
            };
            let should_succeed = matches!(target, VersionTarget::Current) && model.contains(key);
            let result = backend
                .compare_and_swap(key, expected, value, Ttl::Indefinite)
                .await;
            match (result, should_succeed) {
                (Ok(_), true) => {
                    model.put(key, value);
                    Ok(true)
                }
                (Err(ClusterError::CasConflict { .. }), false) => Ok(false),
                (Ok(_), false) => Err(fail(
                    i,
                    key,
                    "compare_and_swap succeeded but the model expected a conflict",
                )),
                (Err(ClusterError::CasConflict { .. }), true) => Err(fail(
                    i,
                    key,
                    "compare_and_swap conflicted but the model expected success",
                )),
                (Err(other), _) => Err(fail(
                    i,
                    key,
                    &format!("compare_and_swap errored: {other:?}"),
                )),
            }
        }
        CacheOp::Delete { key } => {
            let existed = backend
                .delete(key)
                .await
                .map_err(|e| fail(i, key, &format!("delete errored: {e:?}")))?;
            let model_existed = model.remove(key);
            if existed != model_existed {
                return Err(fail(
                    i,
                    key,
                    &format!("delete existed={existed} but model expected {model_existed}"),
                ));
            }
            Ok(existed)
        }
    }
}

/// Asserts the backend's observed state for the touched key matches the model
/// (value + presence) and the version invariants hold (≠ 0; strictly increasing
/// on a mutation, unchanged otherwise). Updates the per-key watermark.
async fn check_after(
    backend: &dyn ClusterCacheBackend,
    model: &CacheModel,
    marks: &mut Watermarks,
    op: &CacheOp,
    i: usize,
    mutated: bool,
) -> Result<(), String> {
    let key = op.key();
    let observed = backend
        .get(key)
        .await
        .map_err(|e| fail(i, key, &format!("get (post-op) errored: {e:?}")))?;

    match (observed, model.get(key)) {
        (Some(entry), Some(model_value)) => {
            if &entry.value != model_value {
                return Err(fail(
                    i,
                    key,
                    &format!(
                        "value divergence: backend {:?} vs model {model_value:?}",
                        entry.value
                    ),
                ));
            }
            if entry.version == 0 {
                return Err(fail(i, key, "present entry has version 0 (sentinel)"));
            }
            if let Some(&prev) = marks.get(key) {
                if mutated && entry.version <= prev {
                    return Err(fail(
                        i,
                        key,
                        &format!(
                            "version not strictly increasing on mutation: {prev} -> {}",
                            entry.version
                        ),
                    ));
                }
                if !mutated && entry.version != prev {
                    return Err(fail(
                        i,
                        key,
                        &format!(
                            "version changed on a non-mutating op: {prev} -> {}",
                            entry.version
                        ),
                    ));
                }
            }
            marks.insert(key.to_owned(), entry.version);
            Ok(())
        }
        (None, None) => {
            marks.remove(key);
            Ok(())
        }
        (Some(entry), None) => Err(fail(
            i,
            key,
            &format!(
                "backend has the key (v{}) but the model deleted it",
                entry.version
            ),
        )),
        (None, Some(model_value)) => Err(fail(
            i,
            key,
            &format!("backend is missing the key but the model holds {model_value:?}"),
        )),
    }
}

/// Formats an invariant-violation message tagged with the op index and key.
fn fail(index: usize, key: &str, reason: &str) -> String {
    format!("op #{index} on key {key:?}: {reason}")
}
