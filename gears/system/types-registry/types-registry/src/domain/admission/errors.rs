//! Admission failures shared by unit evaluation and worker orchestration.

use serde_json::json;
use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::AdmissionFailureReason;
use super::drift::VectorDrift;
use crate::domain::gts_store::StoreBuildError;

/// Infrastructure failures; [`Self::transient`] classifies retryability.
/// `#[non_exhaustive]` lets T13, T15, T17, T19 and T20 add failures without
/// breaking downstream matches.
#[domain_model]
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkerError {
    #[error("operation {operation_id} does not exist")]
    OperationNotFound { operation_id: Uuid },
    #[error("operation item {item_id} carries no request payload")]
    MissingPayload { item_id: i64 },
    /// The dry-run overlay has no terminal item write after a successful admission.
    #[error("the commit path for operation item {item_id} recorded no terminal item write")]
    MissingItemWrite { item_id: i64 },
    /// Worker bug: `order_batch` partitions ordered/cyclic candidates so every
    /// prediction slot must be written exactly once; one was left unfilled.
    #[error("the dry-run pass left operation item {item_id} without a prediction")]
    MissingPrediction { item_id: i64 },
    /// Rolls back entity writes behind an already-terminal item. The worker
    /// catches this and reports the other pass's outcome; callers never see it.
    #[error("operation item {item_id} was terminalized by another pass")]
    ItemAlreadyTerminal { item_id: i64 },
    #[error("building the transient store failed: {0}")]
    StoreBuild(#[source] StoreBuildError),
    #[error("the blocking evaluation task failed: {0}")]
    EvaluationTask(#[source] tokio::task::JoinError),
    /// An Instance's conforming Type Schema has no committed current revision.
    /// Retryable: redelivery can see the type once committed, avoiding dependence
    /// on submission order. Before T21's outbox this surfaced inline as an opaque
    /// `500`, like write contention surfaced as a storage error.
    #[error("instance '{gts_id}' conforms to '{type_id}', which has no current revision")]
    ConformingTypeAbsent { gts_id: String, type_id: String },
    /// Missing or wrong-kind current-state row. D3 writes entity, revision and
    /// current state atomically, so this is corruption, not a race or candidate fault.
    #[error("entity '{gts_id}' (id {entity_id}) has no current-state row of its kind")]
    CurrentStateMissing { gts_id: String, entity_id: i64 },
    /// An `entity` row vanished between reads in one transaction, though admission
    /// never deletes it. Unlike [`Self::CurrentStateMissing`], the missing row is
    /// in `entity`, not its `type_schema` / `instance` projection.
    #[error("entity '{gts_id}' (id {entity_id}) vanished mid-transaction")]
    EntityVanished { gts_id: String, entity_id: i64 },
    /// A stored `gts_id` no longer parses despite acceptance-time canonicalization.
    /// Report corruption: deriving the required Registry Reference is impossible.
    #[error("operation item {item_id} holds an unparsable stored identifier '{gts_id}': {reason}")]
    StoredIdentifierUnparsable {
        item_id: i64,
        gts_id: String,
        reason: String,
    },
    /// Invalid JSON in a stored baseline indicates corruption, not a candidate refusal.
    #[error("the stored baseline document for '{gts_id}' is not valid JSON: {source}")]
    BaselineUnparsable {
        gts_id: String,
        #[source]
        source: serde_json::Error,
    },
    /// A resolved edge target disappeared before commit.
    #[error("dependency target '{gts_id}' vanished before its edge was committed")]
    DependencyTargetAbsent { gts_id: String },
    /// The entity version is a monotonic persisted identity and cannot be
    /// advanced beyond the storage type's ceiling.
    #[error("entity '{gts_id}' cannot advance resource_version after i64::MAX")]
    ResourceVersionExhausted { gts_id: String },
    /// The revision counter is part of persisted identity and must never wrap or
    /// saturate onto the current revision number. The surrounding transaction
    /// rolls the already-executed resource-version CAS back on this error.
    #[error("entity '{gts_id}' cannot allocate a revision after i32::MAX")]
    RevisionNumberExhausted { gts_id: String },
    /// A candidate refusal discovered after the commit transaction began writing.
    #[error("the revision was refused after its writes began: {0}")]
    RefusedAfterWrite(ItemFailure),
    /// Commit-time revision-vector drift (D4, SPEC §8.1 step 4.3).
    #[error("the evaluation is stale and must be redone: {0}")]
    RevalidationRequired(VectorDrift),
    #[error("storage failure during admission: {0}")]
    Storage(#[from] ScopeError),
    #[error("database failure during admission: {0}")]
    Db(#[from] DbError),
}

impl WorkerError {
    /// Whether redelivery can change the outcome. Exhaustive matching forces an
    /// explicit decision for every new variant, despite `#[non_exhaustive]`.
    ///
    /// Retries cover [`Self::Storage`]/[`Self::Db`] contention, transport or scope
    /// validation; an uncommitted [`Self::ConformingTypeAbsent`]; stale evaluation
    /// ([`Self::RevalidationRequired`]); a concurrently removed
    /// [`Self::DependencyTargetAbsent`]; or a cancelled/lost [`Self::EvaluationTask`].
    ///
    /// [`Self::StoreBuild`] splits by its inner error: a failed closure read is
    /// contention, the parse and shape failures are about the stored data.
    ///
    /// Corruption, invariant violations, exhausted counters and prior refusals
    /// cannot improve on reread. Neither can a missing operation: it commits with
    /// its message. These dead-letter on the first attempt, remaining visible in
    /// the dead-letter table without blocking the single admission partition.
    #[must_use]
    pub const fn transient(&self) -> bool {
        match self {
            Self::Storage(_)
            | Self::Db(_)
            | Self::ConformingTypeAbsent { .. }
            | Self::RevalidationRequired(_)
            | Self::DependencyTargetAbsent { .. }
            | Self::EvaluationTask(_) => true,
            // Not the whole variant: `StoreBuildError` wraps a `ScopeError` for a
            // failed closure read, which is the same contention `Self::Storage`
            // retries. Its other variants are about the stored data.
            Self::StoreBuild(e) => e.is_transient(),
            Self::OperationNotFound { .. }
            | Self::MissingPayload { .. }
            | Self::MissingItemWrite { .. }
            | Self::MissingPrediction { .. }
            | Self::ItemAlreadyTerminal { .. }
            | Self::CurrentStateMissing { .. }
            | Self::EntityVanished { .. }
            | Self::StoredIdentifierUnparsable { .. }
            | Self::BaselineUnparsable { .. }
            | Self::ResourceVersionExhausted { .. }
            | Self::RevisionNumberExhausted { .. }
            | Self::RefusedAfterWrite(_) => false,
        }
    }
}

/// A candidate-level failure: final, recorded, and never retried.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemFailure {
    /// A stable machine reason, preserving unknown codes read from storage.
    pub reason: AdmissionFailureReason,
    pub message: String,
}

impl std::fmt::Display for ItemFailure {
    /// Format as the operator-facing `reason: message` pair.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.reason, self.message)
    }
}

impl ItemFailure {
    #[must_use]
    pub fn new(reason: AdmissionFailureReason, message: String) -> Self {
        Self { reason, message }
    }

    /// The stored `error_payload`: structured, so the reason survives the round
    /// trip as a field rather than as a substring.
    #[must_use]
    pub fn to_payload(&self) -> String {
        json!({ "reason": self.reason.as_str(), "message": self.message }).to_string()
    }

    /// Inverse of [`Self::to_payload`]: preserves `{reason, message}` on redelivery
    /// for T16's per-reason metrics instead of reporting `recorded` with JSON in
    /// `message`. REST reads the row's `error_payload` directly. Invalid payloads
    /// remain verbatim with a diagnostic reason so corruption stays visible.
    #[must_use]
    pub fn from_payload(payload: &str) -> Self {
        match serde_json::from_str::<serde_json::Value>(payload) {
            Ok(value) => {
                let reason = value.get("reason").and_then(serde_json::Value::as_str);
                let message = value.get("message").and_then(serde_json::Value::as_str);
                match (reason, message) {
                    (Some(reason), Some(message)) => Self {
                        reason: AdmissionFailureReason::from_wire(reason),
                        message: message.to_owned(),
                    },
                    _ => Self::new(
                        AdmissionFailureReason::UnrecognizedPayload,
                        payload.to_owned(),
                    ),
                }
            }
            Err(_) => Self::new(
                AdmissionFailureReason::UnparsablePayload,
                payload.to_owned(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The split exists because `StoreBuildError` carries both a contention error
    /// and statements about stored data. Treating the whole variant as permanent
    /// dead-letters an operation a redelivery would have admitted.
    #[test]
    fn a_failed_closure_read_inside_store_build_is_retryable() {
        let contention = WorkerError::StoreBuild(StoreBuildError::Storage(ScopeError::Invalid(
            "connection reset",
        )));

        assert!(
            contention.transient(),
            "a closure read that failed on contention must be retried, not dead-lettered",
        );
    }

    #[test]
    fn a_corrupt_document_inside_store_build_is_permanent() {
        let corrupt = WorkerError::StoreBuild(StoreBuildError::MissingDocument {
            gts_id: "cf.core.example.type.v1~".to_owned(),
        });

        assert!(
            !corrupt.transient(),
            "no redelivery rewrites a missing stored document",
        );
    }
}
