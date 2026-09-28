//! Admission failures shared by unit evaluation and worker orchestration.

use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::AdmissionFailureReason;
use super::drift::VectorDrift;
use crate::domain::dependency::DependencyEdge;
use crate::domain::enums::DependencyKind;
use crate::domain::gts_store::StoreBuildError;

/// Infrastructure failures with retry classification.
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
    /// `order_batch` left a prediction slot unwritten.
    #[error("the dry-run pass left operation item {item_id} without a prediction")]
    MissingPrediction { item_id: i64 },
    /// Internal rollback after another pass terminalized the item.
    #[error("operation item {item_id} was terminalized by another pass")]
    ItemAlreadyTerminal { item_id: i64 },
    /// A concurrent pass won the item CAS, but its outcome row is missing.
    #[error("operation item {item_id} lost the outcome a concurrent pass recorded")]
    ItemOutcomeVanished { item_id: i64 },
    #[error("building the transient store failed: {0}")]
    StoreBuild(#[source] StoreBuildError),
    #[error("the blocking evaluation task failed: {0}")]
    EvaluationTask(#[source] tokio::task::JoinError),
    /// Missing or wrong-kind current-state row after an atomic D3 write.
    #[error("entity '{gts_id}' (id {entity_id}) has no current-state row of its kind")]
    CurrentStateMissing { gts_id: String, entity_id: i64 },
    /// An entity row vanished between reads in one transaction.
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
    /// A failure could not be encoded as its stored `error_payload`.
    #[error("an item failure could not be encoded for storage: {0}")]
    FailureUnencodable(#[source] serde_json::Error),
    #[error("storage failure during admission: {0}")]
    Storage(#[from] ScopeError),
    #[error("database failure during admission: {0}")]
    Db(#[from] DbError),
}

impl WorkerError {
    /// Return whether redelivery may clear this infrastructure failure.
    #[must_use]
    pub fn transient(&self) -> bool {
        match self {
            Self::Storage(error) => crate::domain::retry::scoped_failure_may_clear(error),
            Self::Db(error) => crate::domain::retry::database_failure_may_clear(error),
            Self::StoreBuild(error) => error.is_transient(),
            Self::RevalidationRequired(_) => true,
            // Cancellation is recoverable; a panic is not.
            Self::EvaluationTask(error) => error.is_cancelled(),
            Self::OperationNotFound { .. }
            | Self::MissingPayload { .. }
            | Self::MissingItemWrite { .. }
            | Self::MissingPrediction { .. }
            | Self::ItemAlreadyTerminal { .. }
            | Self::ItemOutcomeVanished { .. }
            | Self::CurrentStateMissing { .. }
            | Self::EntityVanished { .. }
            | Self::StoredIdentifierUnparsable { .. }
            | Self::BaselineUnparsable { .. }
            | Self::DependencyTargetAbsent { .. }
            | Self::ResourceVersionExhausted { .. }
            | Self::RevisionNumberExhausted { .. }
            | Self::RefusedAfterWrite(_)
            | Self::FailureUnencodable(_) => false,
        }
    }

    /// Safe, bounded diagnostic code. Never formats SQL, documents or credentials.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::OperationNotFound { .. } => "operation_not_found",
            Self::MissingPayload { .. } => "missing_payload",
            Self::MissingItemWrite { .. } => "missing_item_write",
            Self::MissingPrediction { .. } => "missing_prediction",
            Self::ItemAlreadyTerminal { .. } => "unexpected_terminal_item",
            Self::ItemOutcomeVanished { .. } => "item_outcome_vanished",
            Self::StoreBuild(_) => "store_build_failed",
            Self::EvaluationTask(_) => "evaluation_task_failed",
            Self::CurrentStateMissing { .. } => "current_state_missing",
            Self::EntityVanished { .. } => "entity_vanished",
            Self::StoredIdentifierUnparsable { .. } => "stored_identifier_unparsable",
            Self::BaselineUnparsable { .. } => "baseline_unparsable",
            Self::DependencyTargetAbsent { .. } => "dependency_target_vanished",
            Self::ResourceVersionExhausted { .. } => "resource_version_exhausted",
            Self::RevisionNumberExhausted { .. } => "revision_number_exhausted",
            Self::RefusedAfterWrite(_) => "unhandled_candidate_refusal",
            Self::RevalidationRequired(_) => "revalidation_required",
            Self::FailureUnencodable(_) => "failure_unencodable",
            Self::Storage(_) => "storage_failure",
            Self::Db(_) => "database_failure",
        }
    }
}

/// Dependency details preserved from a stored failure payload.
/// `kind` stays a string for forward compatibility with newer writers.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureDependency {
    pub kind: String,
    pub target: String,
}

impl From<DependencyEdge> for FailureDependency {
    fn from(edge: DependencyEdge) -> Self {
        Self {
            kind: wire_kind(edge.kind).to_owned(),
            target: edge.target,
        }
    }
}

/// The payload token for a known dependency kind.
const fn wire_kind(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Derivation => "base",
        DependencyKind::InstanceOf => "conforming_type",
        DependencyKind::SchemaRef => "ref",
    }
}

/// A candidate-level failure: final, recorded, and never retried.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemFailure {
    /// A stable machine reason, preserving unknown codes read from storage.
    pub reason: AdmissionFailureReason,
    pub message: String,
    /// Identifies a missing dependency without asking clients to parse the message.
    pub dependency: Option<FailureDependency>,
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
        Self {
            reason,
            message,
            dependency: None,
        }
    }

    /// A missing dependency is a final candidate refusal, not a delivery failure.
    #[must_use]
    pub fn missing_dependency(dependency: DependencyEdge) -> Self {
        let role = match dependency.kind {
            DependencyKind::Derivation => "base type",
            DependencyKind::InstanceOf => "conforming type",
            DependencyKind::SchemaRef => "$ref target",
        };
        Self {
            reason: AdmissionFailureReason::DependencyNotFound,
            message: format!("{role} '{}' is not registered", dependency.target),
            dependency: Some(dependency.into()),
        }
    }

    /// The stored `error_payload`: structured, so the reason survives the round
    /// trip as a field rather than as a substring.
    ///
    /// # Errors
    /// The `serde_json` error if the payload cannot be encoded.
    pub fn to_payload(&self) -> Result<String, serde_json::Error> {
        let dependency = self.dependency.as_ref();
        StoredFailure {
            reason: self.reason.as_str().to_owned(),
            message: self.message.clone(),
            dependency_id: dependency.map(|d| d.target.clone()),
            dependency_kind: dependency.map(|d| d.kind.clone()),
            error_code: None,
            operation_id: None,
        }
        .to_payload()
    }

    /// Parse stored failures while preserving invalid payloads as diagnostics.
    #[must_use]
    pub fn from_payload(payload: &str) -> Self {
        match StoredFailure::parse(payload) {
            Ok(stored) => Self {
                reason: AdmissionFailureReason::from_wire(&stored.reason),
                message: stored.message,
                dependency: stored
                    .dependency_id
                    .zip(stored.dependency_kind)
                    .map(|(target, kind)| FailureDependency { kind, target }),
            },
            Err(unreadable) => Self::new(unreadable.reason, payload.to_owned()),
        }
    }
}

/// A stored `error_payload`, in every field: each writer serializes this type and
/// [`StoredFailure::parse`] reads it, so writer and reader cannot drift apart.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct StoredFailure {
    pub reason: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_id: Option<String>,
    /// Kept as written: newer writers may add kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_kind: Option<String>,
    /// Stable diagnostic code of a `system_failure`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<Uuid>,
}

/// A stored payload [`StoredFailure::parse`] refused, and why.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnreadableFailure {
    /// `unparsable_payload` or `unrecognized_payload`.
    pub reason: AdmissionFailureReason,
    pub cause: String,
}

impl StoredFailure {
    /// A `system_failure`: stable codes only, never infrastructure error text.
    #[must_use]
    pub fn system_failure(operation_id: Uuid, error_code: &str) -> Self {
        Self {
            reason: AdmissionFailureReason::SystemFailure.as_str().to_owned(),
            message: "admission could not complete because of a system failure".to_owned(),
            dependency_id: None,
            dependency_kind: None,
            error_code: Some(error_code.to_owned()),
            operation_id: Some(operation_id),
        }
    }

    /// The stored `error_payload` text.
    ///
    /// # Errors
    /// The `serde_json` error if the payload cannot be encoded.
    pub fn to_payload(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// # Errors
    /// [`UnreadableFailure`] for text that is not JSON, or JSON of another shape,
    /// including a dependency without both halves.
    pub fn parse(payload: &str) -> Result<Self, UnreadableFailure> {
        let unreadable = |reason, cause: String| UnreadableFailure { reason, cause };
        match serde_json::from_str::<Self>(payload) {
            Ok(stored) if stored.dependency_id.is_some() == stored.dependency_kind.is_some() => {
                Ok(stored)
            }
            Ok(_) => Err(unreadable(
                AdmissionFailureReason::UnrecognizedPayload,
                "dependency_id and dependency_kind must appear together".to_owned(),
            )),
            Err(e) if e.is_data() => Err(unreadable(
                AdmissionFailureReason::UnrecognizedPayload,
                e.to_string(),
            )),
            Err(e) => Err(unreadable(
                AdmissionFailureReason::UnparsablePayload,
                e.to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_scope_is_not_a_temporary_database_failure() {
        assert!(!WorkerError::Storage(ScopeError::Invalid("invalid scope")).transient());
        assert!(!WorkerError::Storage(ScopeError::Denied("not allowed")).transient());
        assert!(
            !WorkerError::StoreBuild(StoreBuildError::Storage(ScopeError::Invalid(
                "invalid scope"
            )))
            .transient()
        );
    }

    /// Configuration failures must not inherit the retry default.
    #[test]
    fn a_database_configuration_error_is_permanent() {
        assert!(
            !WorkerError::Db(DbError::InvalidConfig("invalid configuration".into())).transient()
        );
    }

    #[test]
    fn a_target_disappearing_after_evaluation_is_an_invariant_failure() {
        assert!(
            !WorkerError::DependencyTargetAbsent {
                gts_id: "missing".into()
            }
            .transient()
        );
    }

    /// `StoreBuildError` mixes retryable contention with permanent data errors.
    #[test]
    fn a_failed_closure_read_inside_store_build_is_retryable() {
        let contention = WorkerError::StoreBuild(StoreBuildError::Storage(ScopeError::Db(
            sea_orm::DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::Timeout),
        )));

        assert!(
            contention.transient(),
            "a closure read that failed on contention must be retried, not dead-lettered",
        );
    }

    /// Each writer serializes [`StoredFailure`], so the reader gets back every field.
    #[test]
    fn every_stored_failure_writer_round_trips_through_the_reader() -> Result<(), serde_json::Error>
    {
        let operation_id = Uuid::from_u128(7);
        let system = StoredFailure::system_failure(operation_id, "storage_failure");
        let payload = system.to_payload()?;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&payload)?,
            serde_json::json!({
                "reason": "system_failure",
                "message": "admission could not complete because of a system failure",
                "error_code": "storage_failure",
                "operation_id": operation_id,
            }),
        );
        assert_eq!(StoredFailure::parse(&payload), Ok(system));

        let missing = ItemFailure::missing_dependency(DependencyEdge {
            kind: DependencyKind::SchemaRef,
            target: "cf.core.absent.type.v1~".to_owned(),
        });
        let stored = StoredFailure::parse(&missing.to_payload()?);
        assert_eq!(
            stored.map(|s| (s.dependency_id, s.dependency_kind)),
            Ok((
                Some("cf.core.absent.type.v1~".to_owned()),
                Some("ref".to_owned())
            )),
        );
        Ok(())
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
