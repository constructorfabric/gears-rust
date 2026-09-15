//! Minimal database-backed Types Registry contract.
//!
//! This is the transport-agnostic subset needed by startup reconcilers while
//! the legacy [`crate::TypesRegistryClient`] still serves the process-local
//! registry. Unlike that legacy trait, implementations of this contract must
//! read and write the authoritative persistent registry.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

/// Persisted entity kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityKind {
    TypeSchema,
    Instance,
}

/// Persisted entity lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleStatus {
    Active,
    Deleted,
}

/// Authoritative persisted representation returned by an exact read.
#[derive(Clone, Debug, PartialEq)]
pub struct EntitySnapshot {
    pub gts_id: String,
    pub gts_uuid: Uuid,
    pub kind: EntityKind,
    pub lifecycle_status: LifecycleStatus,
    pub resource_version: i64,
    pub owning_gear: Option<String>,
    pub content: Option<Value>,
    pub resolved_schema: Option<Value>,
    pub effective_traits: Option<Value>,
    pub effective_traits_schema: Option<Value>,
}

/// One candidate in a persistent registration operation.
#[derive(Clone, Debug, PartialEq)]
pub struct RegisterItem {
    pub gts_id: String,
    pub content: Value,
    /// `None` means that the identifier must not exist.
    pub expected_resource_version: Option<i64>,
    pub force: bool,
}

/// Persistent registration request.
#[derive(Clone, Debug, PartialEq)]
pub struct RegisterEntities {
    pub items: Vec<RegisterItem>,
    pub dry_run: bool,
    /// Trusted in-process ownership attribution. This is metadata, never an
    /// authorization input.
    pub owning_gear: String,
}

/// Progress of a persistent registration operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationStatus {
    Pending,
    Running,
    Completed,
}

/// Terminal or in-progress state of one registration candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateStatus {
    Pending,
    Running,
    Succeeded,
    Unchanged,
    Failed,
}

/// Structured terminal failure stored for one candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateError {
    pub reason: String,
    pub message: String,
}

/// Result for one candidate in a persistent operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationItemResult {
    pub gts_id: String,
    pub status: CandidateStatus,
    pub resource_version: Option<i64>,
    pub error: Option<CandidateError>,
}

/// Persistent registration operation and its per-candidate outcomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationOperation {
    pub operation_id: Uuid,
    pub status: OperationStatus,
    pub items: Vec<RegistrationItemResult>,
}

/// Authoritative persistent registry API used by startup reconciliation.
///
/// The legacy `TypesRegistryClient` remains available during the P0 migration,
/// but must not be used to prove persistence or cross-replica convergence.
#[async_trait]
pub trait TypesRegistryEntities: Send + Sync {
    /// Read an entity directly from authoritative storage.
    async fn get_entity(&self, gts_id: &str) -> Result<Option<EntitySnapshot>, CanonicalError>;

    /// Atomically replace ownership metadata when both the current owner and
    /// resource version match. A `false` result is an ordinary lost race.
    async fn compare_and_swap_owning_gear(
        &self,
        gts_id: &str,
        expected_resource_version: i64,
        expected_owning_gear: Option<String>,
        owning_gear: String,
    ) -> Result<bool, CanonicalError>;

    /// Register candidates and wait until their operation is terminal.
    ///
    /// Implementations must reuse `idempotency_key` for transport retries and
    /// stop waiting after `wait_timeout`. Candidate-level admission failures
    /// are returned in the operation rather than as the outer error.
    async fn register_and_await(
        &self,
        idempotency_key: String,
        request: RegisterEntities,
        wait_timeout: Duration,
    ) -> Result<RegistrationOperation, CanonicalError>;
}
