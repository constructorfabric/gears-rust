//! Admission failures shared by unit evaluation and worker orchestration.

use serde_json::json;
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
            | Self::RefusedAfterWrite(_) => false,
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
    #[must_use]
    pub fn to_payload(&self) -> String {
        let mut payload = json!({ "reason": self.reason.as_str(), "message": self.message });
        if let Some(dependency) = &self.dependency {
            payload["dependency_id"] = json!(dependency.target);
            payload["dependency_kind"] = json!(dependency.kind);
        }
        payload.to_string()
    }

    /// Parse stored failures while preserving invalid payloads as diagnostics.
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
                        // Preserve unknown kinds written by newer versions.
                        dependency: value
                            .get("dependency_id")
                            .and_then(serde_json::Value::as_str)
                            .zip(
                                value
                                    .get("dependency_kind")
                                    .and_then(serde_json::Value::as_str),
                            )
                            .map(|(target, kind)| FailureDependency {
                                kind: kind.to_owned(),
                                target: target.to_owned(),
                            }),
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
