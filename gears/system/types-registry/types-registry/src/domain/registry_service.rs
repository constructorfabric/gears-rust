//! Transport-neutral, database-backed registry service (SPEC §8.4).
//! Submissions are accepted here and admitted by the outbox.
//! P0 managed entities are unrestricted, but ports already accept an access scope.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, ScopeError};
use toolkit_db::{DBProvider, Db, DbError};
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::config::TypesRegistryConfig;
use crate::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use crate::domain::admission::worker::{Tuning, WorkerError, run_operation};
use crate::domain::admission::{
    Accepted, AdmissionFailureReason, Candidate, OperationDispatch, SubmitRequest,
};
use crate::domain::enums::{
    EntityKind, LifecycleStatus, OperationItemStatus, OperationKind, OperationStatus,
};
use crate::domain::policy::RegistrationPolicy;
use crate::domain::ports::metrics::{AdmissionMetrics, PassLabels, RefusalStage};
use crate::domain::ports::{
    CurrentDocument, CurrentInstanceValue, CurrentTypeSchemaRow, Stores, snapshot_read,
};

/// GTS identifier or deterministic Registry Reference for the same row.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntityKey {
    GtsId(String),
    Uuid(Uuid),
}

impl EntityKey {
    /// Parse a UUID as a Registry Reference; otherwise keep the GTS identifier.
    #[must_use]
    pub fn parse(key: &str) -> Self {
        match Uuid::parse_str(key) {
            Ok(uuid) => Self::Uuid(uuid),
            Err(_) => Self::GtsId(key.to_owned()),
        }
    }
}

/// Deletion target shared by single and batch requests.
#[domain_model]
#[derive(Clone, Debug)]
pub struct DeleteTarget {
    pub key: EntityKey,
    /// Required positive version, validated during acceptance.
    pub expected_resource_version: Option<i64>,
}

/// A submitted deletion, before its keys are resolved to identifiers.
#[domain_model]
#[derive(Clone, Debug)]
pub struct DeleteRequest {
    /// Required; optional only to share acceptance validation.
    pub idempotency_key: Option<String>,
    pub dry_run: bool,
    pub targets: Vec<DeleteTarget>,
}

/// One operation and its per-candidate outcomes, as a caller polls it.
#[domain_model]
#[derive(Clone, Debug)]
pub struct OperationRecord {
    pub operation_id: Uuid,
    pub kind: OperationKind,
    pub dry_run: bool,
    pub status: OperationStatus,
    pub created_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub completed_at: Option<OffsetDateTime>,
    pub items: Vec<OperationItemRecord>,
}

/// One candidate's durable outcome.
#[domain_model]
#[derive(Clone, Debug)]
pub struct OperationItemRecord {
    pub gts_id: String,
    pub status: OperationItemStatus,
    pub resource_version: Option<i64>,
    /// The stored structured reason, verbatim.
    pub error: Option<String>,
}

/// One entity with its content and D3's materialized artifacts.
#[domain_model]
#[derive(Clone, Debug)]
pub struct EntityRecord {
    pub gts_id: String,
    pub gts_uuid: Uuid,
    pub kind: EntityKind,
    pub lifecycle_status: LifecycleStatus,
    pub resource_version: i64,
    pub owning_gear: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// The authored document under the public resource name. Absent only if the
    /// current-state row is missing, which is a corrupt row rather than a state a
    /// reader should expect.
    pub content: Option<Value>,
    pub resolved_schema: Option<Value>,
    pub effective_traits: Option<Value>,
    pub effective_traits_schema: Option<Value>,
}

/// Kind-specific state that prevents invalid artifact combinations.
enum CurrentState {
    /// The authored document and D3's materialized artifacts.
    TypeSchema {
        current: Option<CurrentTypeSchemaRow>,
        document: Option<CurrentDocument>,
    },
    /// The authored value. No artifact, because an Instance has none.
    Instance { value: Option<CurrentInstanceValue> },
}

/// What the service can fail with. One layer above the two admission halves, so a
/// transport adapter maps one type.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Acceptance(#[from] AcceptanceError),
    #[error(transparent)]
    Worker(#[from] WorkerError),
    #[error("storage failure: {0}")]
    Storage(#[from] ScopeError),
    #[error("database failure: {0}")]
    Db(#[from] DbError),
    #[error("a stored document could not be read as JSON: {0}")]
    CorruptDocument(String),
    /// Registry Reference with no identifier for an asynchronous item outcome.
    #[error("no entity has Registry Reference {gts_uuid}")]
    UnresolvedReference { gts_uuid: Uuid },
}

impl ServiceError {
    /// Return an exhaustive, log-safe cause kind without formatting sensitive data.
    #[must_use]
    pub const fn cause_kind(&self) -> &'static str {
        match self {
            Self::Acceptance(_) => "acceptance",
            Self::Worker(_) => "worker",
            Self::Storage(_) => "storage",
            Self::Db(_) => "database",
            Self::CorruptDocument(_) => "corrupt_document",
            Self::UnresolvedReference { .. } => "unresolved_reference",
        }
    }
}

/// The database-backed registry service.
#[domain_model]
pub struct RegistryService {
    db: Db,
    /// Injected ports keep `SeaORM` out of the domain.
    stores: Arc<dyn Stores>,
    policy: RegistrationPolicy,
    config: TypesRegistryConfig,
    dispatch: Arc<dyn OperationDispatch>,
    /// The admission instruments (T16).
    metrics: Arc<dyn AdmissionMetrics>,
}

impl RegistryService {
    #[must_use]
    pub fn new(
        db: Db,
        stores: Arc<dyn Stores>,
        policy: RegistrationPolicy,
        config: TypesRegistryConfig,
        dispatch: Arc<dyn OperationDispatch>,
        metrics: Arc<dyn AdmissionMetrics>,
    ) -> Self {
        Self {
            db,
            stores,
            policy,
            config,
            dispatch,
            metrics,
        }
    }

    /// Admission budget, also used by the outbox leased handler.
    pub(crate) fn operation_timeout(&self) -> std::time::Duration {
        self.config.worker.operation_timeout
    }

    /// Delivery attempts the outbox handler may spend on one operation.
    pub(crate) fn max_delivery_attempts(&self) -> u32 {
        self.config.worker.max_delivery_attempts
    }

    /// Instruments for outcomes outside the service, such as outbox delivery.
    pub(crate) fn metrics(&self) -> &dyn AdmissionMetrics {
        self.metrics.as_ref()
    }

    /// Fail undecided items and terminalize the operation as a system failure.
    ///
    /// # Errors
    /// [`ServiceError::Storage`] or [`ServiceError::Db`] if the write fails.
    pub(crate) async fn record_system_failure(
        &self,
        operation_id: Uuid,
        now: OffsetDateTime,
        error_code: &'static str,
    ) -> Result<(), ServiceError> {
        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let stores = Arc::clone(&self.stores);
        let scope = Self::scope();
        // Expose stable codes, never infrastructure error text.
        let payload = serde_json::json!({
            "reason": AdmissionFailureReason::SystemFailure.as_str(),
            "message": "admission could not complete because of a system failure",
            "error_code": error_code,
            "operation_id": operation_id,
        })
        .to_string();
        provider
            .transaction(move |tx| {
                Box::pin(async move {
                    // One guarded statement fits the remaining lease and preserves outcomes.
                    stores
                        .fail_nonterminal_items(tx, &scope, operation_id, payload, now)
                        .await?;
                    // A system failure can move either pending or running operations.
                    stores
                        .mark_system_failed(tx, &scope, operation_id, now)
                        .await?;
                    Ok(())
                })
            })
            .await
    }

    /// See the module docs for the P0 `allow_all` scope.
    fn scope() -> AccessScope {
        AccessScope::allow_all()
    }

    /// Accept a submission and dispatch it durably; the outbox admits it.
    /// Acceptance and dispatch share one transaction, so a committed operation
    /// always has a driver.
    ///
    /// # Errors
    /// [`ServiceError::Acceptance`] for every synchronous refusal, including the
    /// fingerprint conflict.
    pub async fn submit(
        &self,
        request: &SubmitRequest,
        now: OffsetDateTime,
    ) -> Result<Accepted, ServiceError> {
        let provider: DBProvider<AcceptanceError> = DBProvider::new(self.db.clone());
        Ok(accept(
            &self.stores,
            &provider,
            &Self::scope(),
            &AcceptanceContext {
                policy: &self.policy,
                config: &self.config,
                metrics: &self.metrics,
            },
            &self.dispatch,
            request,
            now,
        )
        .await?)
    }

    /// Admit an operation, skipping completed work on redelivery.
    /// Cancellation is recoverable because candidate outcomes commit independently.
    ///
    /// # Errors
    /// [`ServiceError::Worker`] for infrastructure failures. Candidate refusals are
    /// recorded on items and return `Ok`.
    pub async fn admit(&self, operation_id: Uuid, now: OffsetDateTime) -> Result<(), ServiceError> {
        let worker: DBProvider<WorkerError> = DBProvider::new(self.db.clone());
        run_operation(
            &self.stores,
            &worker,
            &Self::scope(),
            Tuning {
                limits: &self.config.limits,
                worker: &self.config.worker,
                metrics: &self.metrics,
                allow_compatibility_force: self.config.allow_compatibility_force,
            },
            operation_id,
            now,
        )
        .await?;
        Ok(())
    }

    /// Submit a single or batch deletion through the shared admission path (SPEC §8.4).
    ///
    /// # Errors
    /// [`ServiceError::UnresolvedReference`] for an unknown Registry Reference,
    /// plus errors from [`Self::submit`].
    pub async fn delete(
        &self,
        request: &DeleteRequest,
        now: OffsetDateTime,
    ) -> Result<Accepted, ServiceError> {
        // Bound Registry Reference lookups before resolving targets.
        let limit = self.config.limits.batch_candidates;
        if request.targets.len() > limit {
            let error = AcceptanceError::BatchTooLarge {
                count: request.targets.len(),
                limit,
            };
            // This refusal never reaches acceptance's metric.
            self.metrics.refused(
                RefusalStage::Acceptance,
                error.reason(),
                PassLabels::new(OperationKind::Deletion, request.dry_run),
            );
            return Err(ServiceError::Acceptance(error));
        }
        let candidates = self.resolve_targets(&request.targets).await?;
        self.submit(
            &SubmitRequest {
                idempotency_key: request.idempotency_key.clone(),
                kind: OperationKind::Deletion,
                dry_run: request.dry_run,
                candidates,
            },
            now,
        )
        .await
    }

    /// Resolve immutable Registry References; admission rechecks mutable state.
    async fn resolve_targets(
        &self,
        targets: &[DeleteTarget],
    ) -> Result<Vec<Candidate>, ServiceError> {
        let references: Vec<Uuid> = targets
            .iter()
            .filter_map(|target| match &target.key {
                EntityKey::Uuid(gts_uuid) => Some(*gts_uuid),
                EntityKey::GtsId(_) => None,
            })
            .collect();
        // Identifier-only batches need no lookup.
        let resolved = if references.is_empty() {
            BTreeMap::new()
        } else {
            self.reverse_resolve(references).await?
        };

        targets
            .iter()
            .map(|target| {
                let gts_id = match &target.key {
                    EntityKey::GtsId(gts_id) => gts_id.clone(),
                    EntityKey::Uuid(gts_uuid) => resolved.get(gts_uuid).cloned().ok_or(
                        ServiceError::UnresolvedReference {
                            gts_uuid: *gts_uuid,
                        },
                    )?,
                };
                Ok(Candidate {
                    gts_id,
                    // Deletion has no content or compatibility check to waive (ADR-0004).
                    content: None,
                    expected_resource_version: target.expected_resource_version,
                    force: false,
                })
            })
            .collect()
    }

    /// Resolve Registry References under one snapshot, in chunked batch reads
    /// rather than one query per reference. The caller has already bounded the
    /// batch by `limits.batch_candidates`. Omit missing rows so the caller reports
    /// the first unresolved target in request order.
    async fn reverse_resolve(
        &self,
        references: Vec<Uuid>,
    ) -> Result<BTreeMap<Uuid, String>, ServiceError> {
        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let scope = Self::scope();
        let stores = Arc::clone(&self.stores);
        provider
            .transaction_with_config(snapshot_read(&self.db), move |tx| {
                Box::pin(async move {
                    // Resolve tombstones too, preserving the identifier path's `not_active` outcome.
                    let rows = stores.find_by_gts_uuids(tx, &scope, &references).await?;
                    Ok(rows
                        .into_iter()
                        .map(|row| (row.gts_uuid, row.gts_id))
                        .collect())
                })
            })
            .await
    }

    /// Read one operation and its per-candidate outcomes.
    ///
    /// # Errors
    /// [`ServiceError::Storage`] for a read failure. An absent operation is
    /// `Ok(None)`, because "not found" is an answer rather than a fault.
    pub async fn operation(&self, id: Uuid) -> Result<Option<OperationRecord>, ServiceError> {
        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let scope = Self::scope();
        let stores = Arc::clone(&self.stores);
        // Worker transactions write status and item outcomes separately; one
        // snapshot prevents combining states that never coexisted.
        let Some((operation, items)) = provider
            .transaction_with_config(snapshot_read(&self.db), move |tx| {
                Box::pin(async move {
                    let Some(operation) = stores.find_by_id(tx, &scope, id).await? else {
                        return Ok(None);
                    };
                    let items = stores.find_items(tx, &scope, id).await?;
                    Ok(Some((operation, items)))
                })
            })
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(OperationRecord {
            operation_id: operation.id,
            kind: operation.kind,
            dry_run: operation.dry_run,
            status: operation.status,
            created_at: operation.created_at,
            started_at: operation.started_at,
            completed_at: operation.completed_at,
            items: items
                .into_iter()
                .map(|item| OperationItemRecord {
                    gts_id: item.gts_id,
                    status: item.status,
                    resource_version: item.result_resource_version,
                    error: item.error_payload,
                })
                .collect(),
        }))
    }

    /// Read one entity by identifier or Registry Reference, with its authored
    /// content and D3's materialized artifacts.
    ///
    /// Branch on row kind, not key: Type Schemas have a document and three D3
    /// artifacts; Instances only an authored value. T9's schema-only read path
    /// would return null `content` for Instances admitted since T10.
    /// DELETED tombstones remain exact-readable but leave discovery.
    ///
    /// # Errors
    /// [`ServiceError::Storage`] for a read failure, or
    /// [`ServiceError::CorruptDocument`] if a stored document is not JSON.
    pub async fn entity(&self, key: &EntityKey) -> Result<Option<EntityRecord>, ServiceError> {
        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let scope = Self::scope();
        let stores = Arc::clone(&self.stores);
        let key = key.clone();

        // One snapshot keeps atomically written `entity.resource_version`, artifacts
        // and authored document together. T11 revisions could otherwise pair N with
        // N + 1 artifacts, breaking T29's version/body promise for conditional reads.
        let Some((row, current)) = provider
            .transaction_with_config(snapshot_read(&self.db), move |tx| {
                Box::pin(async move {
                    let found = match &key {
                        EntityKey::GtsId(gts_id) => {
                            stores.find_by_gts_id(tx, &scope, gts_id).await?
                        }
                        EntityKey::Uuid(uuid) => stores.find_by_gts_uuid(tx, &scope, *uuid).await?,
                    };
                    let Some(row) = found else {
                        return Ok(None);
                    };
                    let current = match row.entity_kind {
                        EntityKind::TypeSchema => CurrentState::TypeSchema {
                            current: stores.find_current_schema(tx, &scope, row.id).await?,
                            document: stores.current_documents(tx, &scope, &[row.id]).await?.pop(),
                        },
                        // `current_values` includes the revision; no `find_current_instance` read needed.
                        EntityKind::Instance => CurrentState::Instance {
                            value: stores.current_values(tx, &scope, &[row.id]).await?.pop(),
                        },
                    };
                    Ok(Some((row, current)))
                })
            })
            .await?
        else {
            return Ok(None);
        };

        let (content, resolved_schema, effective_traits, effective_traits_schema) = match current {
            CurrentState::TypeSchema { current, document } => {
                let current = current.ok_or_else(|| {
                    ServiceError::CorruptDocument(format!(
                        "entity '{}' has no current Type Schema state",
                        row.gts_id
                    ))
                })?;
                let document = document.ok_or_else(|| {
                    ServiceError::CorruptDocument(format!(
                        "entity '{}' has no current Type Schema document",
                        row.gts_id
                    ))
                })?;
                (
                    Some(parse_stored(&document.raw_schema, &row.gts_id)?),
                    Some(parse_stored(&current.resolved_schema, &row.gts_id)?),
                    Some(parse_stored(&current.effective_traits, &row.gts_id)?),
                    Some(parse_stored(&current.effective_traits_schema, &row.gts_id)?),
                )
            }
            // Instances have no derived artifacts; all three are intentionally `None`.
            CurrentState::Instance { value } => {
                let value = value.ok_or_else(|| {
                    ServiceError::CorruptDocument(format!(
                        "entity '{}' has no current Instance state",
                        row.gts_id
                    ))
                })?;
                (
                    Some(parse_stored(&value.canonical_value, &row.gts_id)?),
                    None,
                    None,
                    None,
                )
            }
        };

        Ok(Some(EntityRecord {
            gts_id: row.gts_id,
            gts_uuid: row.gts_uuid,
            kind: row.entity_kind,
            lifecycle_status: row.lifecycle_status,
            resource_version: row.resource_version,
            owning_gear: row.owning_gear,
            created_at: row.created_at,
            updated_at: row.updated_at,
            content,
            resolved_schema,
            effective_traits,
            effective_traits_schema,
        }))
    }
}

fn parse_stored(text: &str, gts_id: &str) -> Result<Value, ServiceError> {
    serde_json::from_str(text)
        .map_err(|e| ServiceError::CorruptDocument(format!("'{gts_id}': {e}")))
}
