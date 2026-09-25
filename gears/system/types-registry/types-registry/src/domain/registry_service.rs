//! Transport-neutral, database-backed registry service (SPEC §8.4).
//! Submissions are accepted here and admitted by the outbox.
//! P0 managed entities are unrestricted, but ports already accept an access scope.

use std::collections::BTreeMap;
use std::num::NonZeroU8;
use std::sync::Arc;

use gts::{GtsId, GtsIdPattern};
use serde_json::value::RawValue;
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, ScopeError};
use toolkit_db::{DBProvider, Db, DbError};
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::config::TypesRegistryConfig;
use crate::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use crate::domain::admission::worker::{Tuning, WorkerError, run_operation};
use crate::domain::admission::{
    Accepted, Candidate, OperationDispatch, StoredFailure, SubmitRequest, UnreadableFailure,
};
use crate::domain::enums::{
    EntityKind, LifecycleFilter, LifecycleStatus, OperationItemStatus, OperationKind,
    OperationStatus,
};
use crate::domain::policy::RegistrationPolicy;
use crate::domain::ports::metrics::{AdmissionMetrics, PassLabels, RefusalStage};
use crate::domain::ports::{
    CurrentReadRow, EntityRow, ListFilter, PageRequest, Stores, snapshot_read,
};
use crate::domain::selection::{EntityField, FieldSelection};

/// GTS identifier or deterministic Registry Reference for the same row.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
    pub error: Option<Result<StoredFailure, UnreadableFailure>>,
}

/// Projected by the selection: an optional field is `Some` only when selected and
/// applicable; a selected JSON `null` is `Some` holding the text `null`. Documents
/// are the stored canonical text, validated but never parsed into a tree.
#[domain_model]
#[derive(Clone, Debug)]
pub struct EntityRecord {
    pub gts_id: String,
    pub gts_uuid: Uuid,
    pub kind: EntityKind,
    pub origin: Option<ManagedOrigin>,
    pub lifecycle_status: LifecycleStatus,
    pub content: Option<Box<RawValue>>,
    pub resolved_schema: Option<Box<RawValue>>,
    pub effective_traits: Option<Box<RawValue>>,
    pub effective_traits_schema: Option<Box<RawValue>>,
    pub provenance: Option<Provenance>,
}

/// Where a managed entity's current state came from.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManagedOrigin {
    pub resource_version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub gts_spec_version: String,
    pub gts_impl_version: String,
    /// `None` for an Instance.
    pub compat_forced: Option<bool>,
}

/// One key's answer in a batch read.
///
/// Absence is a result, not a failure: a caller reconciling a set needs to know
/// which of its keys is missing, and one absent key must not fail the others.
/// P0 has the two states DESIGN's four reduce to — `unchanged` needs T29's
/// validators and `failed` needs federation, which is out of scope (SPEC §10.1).
#[domain_model]
#[derive(Clone, Debug)]
// `Found` already owns heap JSON up to the 1 MB document bound, so the 240 bytes
// `NotFound` pays for the shared discriminant are not what makes a batch large.
// Boxing would buy that back and charge an allocation and an indirection per found
// record — the wrong trade for the variant that carries every successful read.
#[expect(
    clippy::large_enum_variant,
    reason = "boxing Found penalises every successful read with an allocation and an indirection; NotFound's discriminant overhead is acceptable"
)]
pub enum EntityLookup {
    Found(EntityRecord),
    NotFound,
}

/// A discovery query, over active entities unless `lifecycle` says otherwise (D12).
///
/// No origin, availability or scope filter: each is out of P0 scope (SPEC §2) or a
/// tenant-plane input.
#[domain_model]
#[derive(Clone, Debug, Default)]
pub struct DiscoveryQuery {
    /// A GTS pattern: parsed by `gts-rust`, matched in SQL over stored segments.
    pub pattern: Option<String>,
    /// Exclusive keyset lower bound: the position the previous page stopped at.
    pub after: Option<String>,
    /// `None` takes `limits.page_size_default`; above `limits.page_size_max` is refused.
    pub limit: Option<u64>,
    pub kind: Option<EntityKind>,
    pub lifecycle: LifecycleFilter,
    /// Inclusive maximum number of GTS identifier segments.
    pub max_chain_depth: Option<NonZeroU8>,
    pub selection: FieldSelection,
}

/// One bounded discovery page and the position a caller resumes from.
#[domain_model]
#[derive(Clone, Debug)]
pub struct DiscoveryPage {
    pub items: Vec<EntityRecord>,
    /// The page size actually applied: the caller's `limit`, or
    /// `limits.page_size_default` where it named none. Returned rather than left
    /// for a transport adapter to restate, so REST and a future gRPC adapter
    /// cannot report different defaults for the same read.
    pub limit: u32,
    /// The last returned `gts_id` when another match exists, else `None`.
    pub next_after: Option<String>,
}

/// How many keys one batch read may name.
///
/// ponytail: ceiling C10 — **100, not DESIGN §3.3's 500.** DESIGN picked the
/// higher number so a reconciliation could read every identifier it might write
/// before selecting the at most `limits.batch_candidates` (100) it actually
/// submits. P0 gives that headroom up deliberately: one `found` result carries the
/// authored document plus D3's three materialized artifacts, and §3.2 bounds a
/// resolved document at 1 MB, so the ceiling is what bounds a single response —
/// 500 keys is a response this gear should never be asked to build. A
/// reconciliation that wants to inspect more identifiers than it writes pages its
/// reads instead, which the T23 helper owns. The upgrade path is that helper
/// plus a bound on response *bytes* rather than on keys; until then the key count
/// is the only bound there is.
///
/// A constant rather than a configuration key because §10.3's configuration is
/// fixed for P0. Equal to `limits.batch_candidates` today and still not the same
/// bound: a deployment that raises the write ceiling must not silently widen read
/// fan-out with it.
pub const MAX_BATCH_GET_KEYS: usize = 100;

/// A read key's ceiling in bytes: a GTS identifier runs to 1024.
pub const MAX_KEY_LEN: usize = 1024;

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
    /// A blocking task panicked or was cancelled; the source says which.
    #[error("a blocking task did not complete: {0}")]
    Blocking(#[source] tokio::task::JoinError),
    /// Registry Reference with no identifier for an asynchronous item outcome.
    #[error("no entity has Registry Reference {gts_uuid}")]
    UnresolvedReference { gts_uuid: Uuid },
    /// A batch read named no key at all, or more than [`MAX_BATCH_GET_KEYS`].
    #[error(
        "a batch read must name between 1 and {MAX_BATCH_GET_KEYS} keys; this one named {count}"
    )]
    BatchReadOutOfRange { count: usize },
    #[error("a key must be at most {MAX_KEY_LEN} bytes; this one is {len}")]
    KeyTooLong { len: usize },
    /// `limit` outside `1..=limits.page_size_max` (D12).
    #[error("a page size must be between 1 and {max}; this request asked for {limit}")]
    PageSizeOutOfRange { limit: u64, max: u32 },
    /// The discovery pattern is not a GTS identifier pattern.
    #[error("the discovery pattern is not a GTS pattern: {message}")]
    InvalidPattern { message: String },
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
            Self::Blocking(_) => "blocking_task",
            Self::UnresolvedReference { .. } => "unresolved_reference",
            Self::BatchReadOutOfRange { .. } => "batch_read_out_of_range",
            Self::KeyTooLong { .. } => "key_too_long",
            Self::PageSizeOutOfRange { .. } => "page_size_out_of_range",
            Self::InvalidPattern { .. } => "invalid_pattern",
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

    /// The configured limits, for a transport adapter's published contract.
    #[must_use]
    pub fn limits(&self) -> &crate::config::Limits {
        &self.config.limits
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
    /// [`ServiceError::Storage`] or [`ServiceError::Db`] if the write fails, or
    /// [`ServiceError::Worker`] if the failure payload cannot be encoded.
    pub(crate) async fn record_system_failure(
        &self,
        operation_id: Uuid,
        now: OffsetDateTime,
        error_code: &'static str,
    ) -> Result<(), ServiceError> {
        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let stores = Arc::clone(&self.stores);
        let scope = Self::scope();
        let payload = StoredFailure::system_failure(operation_id, error_code)
            .to_payload()
            .map_err(WorkerError::FailureUnencodable)?;
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
                .map(|item| {
                    let error = item.error_payload.as_deref().map(StoredFailure::parse);
                    OperationItemRecord {
                        gts_id: item.gts_id,
                        status: item.status,
                        resource_version: item.result_resource_version,
                        error,
                    }
                })
                .collect(),
        }))
    }

    /// Read one entity by identifier or Registry Reference, projected by
    /// `selection`.
    ///
    /// One key's [`Self::batch_get`], as `delete_entity` is one target's `delete`.
    /// Sharing the implementation is what makes the two surfaces agree: the key is
    /// classified once, an absent row is an absence on both, and an identifier that
    /// cannot exist is not validated by one surface and looked up by the other.
    /// DELETED tombstones stay exact-readable and only leave discovery.
    ///
    /// # Errors
    /// [`ServiceError::Storage`] for a read failure, or
    /// [`ServiceError::CorruptDocument`] if a stored document is not JSON.
    pub async fn entity(
        &self,
        key: &EntityKey,
        selection: FieldSelection,
    ) -> Result<Option<EntityRecord>, ServiceError> {
        let results = self.batch_get(std::slice::from_ref(key), selection).await?;
        Ok(results.into_iter().find_map(|(_, lookup)| match lookup {
            EntityLookup::Found(record) => Some(record),
            EntityLookup::NotFound => None,
        }))
    }

    /// Read a bounded set of keys, answering every one of them (DESIGN §3.3).
    ///
    /// Results are returned in request order with the key each was asked by, so a
    /// caller that mixed identifiers and Registry References matches answers to
    /// questions without re-deriving either. A key named twice collapses onto its
    /// first mention: the answer is per key, not per mention. The two spellings of
    /// one row are **not** duplicates of each other — each is a key a caller asked
    /// about and each is echoed.
    ///
    /// Constant in round trips rather than linear in keys: two identity reads and
    /// two current-state reads per kind, all under one snapshot, whatever the batch
    /// size. Only the documents `selection` names are fetched and parsed.
    ///
    /// # Errors
    /// [`ServiceError::BatchReadOutOfRange`] for an empty or over-long batch,
    /// [`ServiceError::KeyTooLong`] for a key over [`MAX_KEY_LEN`] bytes,
    /// [`ServiceError::Storage`] for a read failure, or
    /// [`ServiceError::CorruptDocument`] if a stored document is not JSON.
    pub async fn batch_get(
        &self,
        keys: &[EntityKey],
        selection: FieldSelection,
    ) -> Result<Vec<(EntityKey, EntityLookup)>, ServiceError> {
        // Bounded before any read, as deletion bounds its batch: the ceiling exists
        // to keep one request's work finite, so it cannot be checked after the work.
        if keys.is_empty() || keys.len() > MAX_BATCH_GET_KEYS {
            return Err(ServiceError::BatchReadOutOfRange { count: keys.len() });
        }
        let mut requested: Vec<EntityKey> = Vec::with_capacity(keys.len());
        let mut seen: std::collections::HashSet<&EntityKey> =
            std::collections::HashSet::with_capacity(keys.len());
        for key in keys {
            if let EntityKey::GtsId(gts_id) = key
                && gts_id.len() > MAX_KEY_LEN
            {
                return Err(ServiceError::KeyTooLong { len: gts_id.len() });
            }
            if seen.insert(key) {
                requested.push(key.clone());
            }
        }

        // A non-canonical identifier cannot be stored, so it never reaches SQL.
        let gts_ids: Vec<String> = requested
            .iter()
            .filter_map(|key| match key {
                EntityKey::GtsId(gts_id) if is_canonical(gts_id) => Some(gts_id.clone()),
                EntityKey::GtsId(_) | EntityKey::Uuid(_) => None,
            })
            .collect();
        let gts_uuids: Vec<Uuid> = requested
            .iter()
            .filter_map(|key| match key {
                EntityKey::Uuid(gts_uuid) => Some(*gts_uuid),
                EntityKey::GtsId(_) => None,
            })
            .collect();
        if gts_ids.is_empty() && gts_uuids.is_empty() {
            return Ok(requested
                .into_iter()
                .map(|key| (key, EntityLookup::NotFound))
                .collect());
        }

        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let scope = Self::scope();
        let stores = Arc::clone(&self.stores);
        // One snapshot keeps each row's atomically written `resource_version`,
        // artifacts and authored document together. T11 revisions could otherwise
        // pair N with N + 1 artifacts, breaking T29's version/body promise for
        // conditional reads.
        let state = provider
            .transaction_with_config(snapshot_read(&self.db), move |tx| {
                Box::pin(async move {
                    let mut rows = stores.find_by_gts_ids(tx, &scope, &gts_ids).await?;
                    rows.extend(stores.find_by_gts_uuids(tx, &scope, &gts_uuids).await?);
                    // The two identity reads are independent, so a row named by both
                    // spellings arrives twice; the current-state reads below must
                    // name it once.
                    rows.sort_by_key(|row| row.id);
                    rows.dedup_by_key(|row| row.id);

                    // Branch on row kind, not on key: Type Schemas have a document
                    // and D3's three artifacts, Instances only an authored value.
                    let current = read_current(&*stores, tx, &scope, &rows, selection).await?;
                    Ok((rows, current))
                })
            })
            .await?;

        let mut records = build_records(state.0, state.1, selection).await?;
        let by_gts_id: BTreeMap<&str, i64> = records
            .iter()
            .map(|(id, record)| (record.gts_id.as_str(), *id))
            .collect();
        let by_gts_uuid: BTreeMap<Uuid, i64> = records
            .iter()
            .map(|(id, record)| (record.gts_uuid, *id))
            .collect();
        let ids: Vec<Option<i64>> = requested
            .iter()
            .map(|key| match key {
                EntityKey::GtsId(gts_id) => by_gts_id.get(gts_id.as_str()).copied(),
                EntityKey::Uuid(gts_uuid) => by_gts_uuid.get(gts_uuid).copied(),
            })
            .collect();
        drop((by_gts_id, by_gts_uuid));
        // Moved out on a row's last mention; cloned only for a row asked by both spellings.
        let mut uses: BTreeMap<i64, usize> = BTreeMap::new();
        for id in ids.iter().flatten() {
            *uses.entry(*id).or_default() += 1;
        }
        Ok(requested
            .into_iter()
            .zip(ids)
            .map(|(key, id)| {
                let record = id.and_then(|id| {
                    let left = uses.get_mut(&id)?;
                    *left -= 1;
                    if *left == 0 {
                        records.remove(&id)
                    } else {
                        records.get(&id).cloned()
                    }
                });
                (
                    key,
                    record.map_or(EntityLookup::NotFound, EntityLookup::Found),
                )
            })
            .collect())
    }

    /// One bounded page of entities in `query.lifecycle`, ordered by canonical identifier and
    /// projected by `query.selection` (D12).
    ///
    /// The page size and its ceiling are deployment configuration, which is why the
    /// default and the refusal live here rather than in a transport adapter: a gRPC
    /// adapter must not be able to page differently from REST.
    ///
    /// # Errors
    /// [`ServiceError::PageSizeOutOfRange`] for a refused `limit`,
    /// [`ServiceError::InvalidPattern`] for a pattern `gts-rust` will not compile,
    /// or [`ServiceError::Storage`] for a read failure.
    pub async fn discover(&self, query: &DiscoveryQuery) -> Result<DiscoveryPage, ServiceError> {
        let max = self.config.limits.page_size_max;
        let limit = match query.limit {
            None => self.config.limits.page_size_default,
            Some(asked) => u32::try_from(asked)
                .ok()
                .filter(|limit| (1..=max).contains(limit))
                .ok_or(ServiceError::PageSizeOutOfRange { limit: asked, max })?,
        };
        // Parsed by `gts-rust`, the sole authority on the pattern grammar
        // (`constraint-gts-implementation`). A string it refuses is a refused
        // request, not an empty page: the two are indistinguishable to a caller
        // that mistyped a wildcard.
        let pattern = query
            .pattern
            .as_deref()
            .map(|raw| {
                GtsIdPattern::try_new(raw).map_err(|e| ServiceError::InvalidPattern {
                    message: e.to_string(),
                })
            })
            .transpose()?;
        let filter = ListFilter {
            pattern,
            kind: query.kind,
            lifecycle: query.lifecycle,
            max_chain_depth: query.max_chain_depth,
        };
        let request = PageRequest {
            after: query.after.clone(),
            limit,
        };

        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let scope = Self::scope();
        let stores = Arc::clone(&self.stores);
        let selection = query.selection;
        let (page, current) = provider
            .transaction_with_config(snapshot_read(&self.db), move |tx| {
                Box::pin(async move {
                    let page = stores.list_page(tx, &scope, &filter, request).await?;
                    let current =
                        read_current(&*stores, tx, &scope, &page.items, selection).await?;
                    Ok((page, current))
                })
            })
            .await?;

        let order: Vec<i64> = page.items.iter().map(|row| row.id).collect();
        let mut records = build_records(page.items, current, selection).await?;
        Ok(DiscoveryPage {
            items: order.iter().filter_map(|id| records.remove(id)).collect(),
            limit,
            next_after: page.next_after,
        })
    }
}

pub(crate) fn is_canonical(gts_id: &str) -> bool {
    GtsId::try_new(gts_id).is_ok_and(|id| id.id() == gts_id)
}

/// The entity ids of one kind, for the kind-specific current-state reads.
fn ids_of(rows: &[EntityRow], kind: EntityKind) -> Vec<i64> {
    rows.iter()
        .filter(|row| row.entity_kind == kind)
        .map(|row| row.id)
        .collect()
}

/// Branches on row kind, not on key: only a Type Schema has artifacts.
async fn read_current(
    stores: &dyn Stores,
    tx: &toolkit_db::DbTx<'_>,
    scope: &AccessScope,
    rows: &[EntityRow],
    selection: FieldSelection,
) -> Result<BTreeMap<i64, CurrentReadRow>, ServiceError> {
    let type_ids = ids_of(rows, EntityKind::TypeSchema);
    let instance_ids = ids_of(rows, EntityKind::Instance);
    let mut current = stores
        .read_current_schemas(tx, scope, &type_ids, selection)
        .await?;
    current.extend(
        stores
            .read_current_values(tx, scope, &instance_ids, selection)
            .await?,
    );
    Ok(current
        .into_iter()
        .map(|row| (row.entity_id, row))
        .collect())
}

/// [`into_records`], off the executor when documents are selected: validating them
/// still scans every byte of up to a page of 1 MB documents.
async fn build_records(
    rows: Vec<EntityRow>,
    current: BTreeMap<i64, CurrentReadRow>,
    selection: FieldSelection,
) -> Result<BTreeMap<i64, EntityRecord>, ServiceError> {
    if !selection.selects_any_document() {
        return into_records(rows, current, selection);
    }
    tokio::task::spawn_blocking(move || into_records(rows, current, selection))
        .await
        .map_err(ServiceError::Blocking)?
}

/// A missing current state, or a selected column that did not come back, is
/// corruption rather than absence, whatever the selection.
fn into_records(
    rows: Vec<EntityRow>,
    mut current: BTreeMap<i64, CurrentReadRow>,
    selection: FieldSelection,
) -> Result<BTreeMap<i64, EntityRecord>, ServiceError> {
    let mut out = BTreeMap::new();
    for row in rows {
        let state = current.remove(&row.id).ok_or_else(|| {
            let what = match row.entity_kind {
                EntityKind::TypeSchema => "current Type Schema state",
                EntityKind::Instance => "current Instance state",
            };
            missing_state(&row.gts_id, what)
        })?;
        let is_schema = row.entity_kind == EntityKind::TypeSchema;
        let document = |field: EntityField, text: Option<String>| {
            select_document(selection, field, is_schema, text, &row.gts_id)
        };
        let content = document(EntityField::Content, state.content)?;
        let resolved_schema = document(EntityField::ResolvedSchema, state.resolved_schema)?;
        let effective_traits = document(EntityField::EffectiveTraits, state.effective_traits)?;
        let effective_traits_schema = document(
            EntityField::EffectiveTraitsSchema,
            state.effective_traits_schema,
        )?;
        let provenance = if selection.contains(EntityField::Provenance) {
            let revision = state
                .provenance
                .ok_or_else(|| missing_state(&row.gts_id, "selected provenance"))?;
            Some(Provenance {
                gts_spec_version: revision.gts_spec_version,
                gts_impl_version: revision.gts_impl_version,
                compat_forced: revision.compat_forced,
            })
        } else {
            None
        };
        out.insert(
            row.id,
            EntityRecord {
                gts_id: row.gts_id,
                gts_uuid: row.gts_uuid,
                kind: row.entity_kind,
                origin: selection
                    .contains(EntityField::Origin)
                    .then_some(ManagedOrigin {
                        resource_version: row.resource_version,
                        created_at: row.created_at,
                        updated_at: row.updated_at,
                    }),
                lifecycle_status: row.lifecycle_status,
                content,
                resolved_schema,
                effective_traits,
                effective_traits_schema,
                provenance,
            },
        );
    }
    Ok(out)
}

fn select_document(
    selection: FieldSelection,
    field: EntityField,
    is_schema: bool,
    text: Option<String>,
    gts_id: &str,
) -> Result<Option<Box<RawValue>>, ServiceError> {
    let applicable = field == EntityField::Content || is_schema;
    if !selection.contains(field) || !applicable {
        return Ok(None);
    }
    let text = text.ok_or_else(|| missing_state(gts_id, field.name()))?;
    raw_stored(text, gts_id).map(Some)
}

fn missing_state(gts_id: &str, what: &str) -> ServiceError {
    ServiceError::CorruptDocument(format!("entity '{gts_id}' has no {what}"))
}

/// Validated, not trusted: a `RawValue` is written to the response verbatim, so
/// text that is not JSON must fail here rather than corrupt the body.
fn raw_stored(text: String, gts_id: &str) -> Result<Box<RawValue>, ServiceError> {
    RawValue::from_string(text)
        .map_err(|e| ServiceError::CorruptDocument(format!("'{gts_id}': {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::enums::OwnershipScope;

    const AT: OffsetDateTime = time::macros::datetime!(2026-09-23 10:00:00 UTC);

    fn row(kind: EntityKind) -> EntityRow {
        EntityRow {
            id: 7,
            gts_uuid: Uuid::nil(),
            gts_id: "gts.cf.core.example.type.v1~".to_owned(),
            entity_kind: kind,
            family_id: 1,
            ownership_scope: OwnershipScope::Global,
            owner_tenant_id: None,
            owning_gear: Some("types-registry".to_owned()),
            lifecycle_status: LifecycleStatus::Active,
            resource_version: 1,
            deleted_at: None,
            created_at: AT,
            updated_at: AT,
        }
    }

    fn state(content: Option<&str>) -> BTreeMap<i64, CurrentReadRow> {
        BTreeMap::from([(
            7,
            CurrentReadRow {
                entity_id: 7,
                content: content.map(str::to_owned),
                resolved_schema: None,
                effective_traits: None,
                effective_traits_schema: None,
                provenance: None,
            },
        )])
    }

    fn content() -> FieldSelection {
        FieldSelection::parse(&["content"]).expect("valid")
    }

    #[test]
    fn a_selected_document_that_did_not_come_back_is_corruption_not_absence() {
        let result = into_records(vec![row(EntityKind::TypeSchema)], state(None), content());
        assert!(
            matches!(result, Err(ServiceError::CorruptDocument(ref d)) if d.contains("content")),
            "{result:?}",
        );
    }

    #[test]
    fn an_unselected_document_is_neither_read_nor_parsed() {
        let records = into_records(
            vec![row(EntityKind::TypeSchema)],
            state(Some("not json")),
            FieldSelection::default(),
        )
        .expect("an unselected column is never parsed");
        assert!(records[&7].content.is_none());
    }

    #[test]
    fn a_selected_json_null_is_kept_as_a_value() {
        let records = into_records(
            vec![row(EntityKind::Instance)],
            state(Some("null")),
            content(),
        )
        .expect("valid");
        assert_eq!(
            records[&7].content.as_deref().map(RawValue::get),
            Some("null")
        );
    }

    #[test]
    fn a_selected_document_that_is_not_json_is_corruption() {
        let result = into_records(
            vec![row(EntityKind::Instance)],
            state(Some("not json")),
            content(),
        );
        assert!(
            matches!(result, Err(ServiceError::CorruptDocument(ref d)) if d.contains("gts.cf.core")),
            "{result:?}",
        );
    }

    #[tokio::test]
    async fn a_failed_blocking_task_keeps_its_panic_classification() {
        let panicked = tokio::spawn(async {
            std::panic::resume_unwind(Box::new("injected record construction panic"));
        })
        .await
        .expect_err("the task panicked");
        let error = ServiceError::Blocking(panicked);
        let source = std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<tokio::task::JoinError>());
        assert!(
            source.is_some_and(tokio::task::JoinError::is_panic),
            "{error:?}"
        );
    }

    #[test]
    fn selected_provenance_that_did_not_come_back_is_corruption() {
        let selection = FieldSelection::parse(&["provenance"]).expect("valid");
        let result = into_records(vec![row(EntityKind::Instance)], state(None), selection);
        assert!(
            matches!(result, Err(ServiceError::CorruptDocument(_))),
            "{result:?}"
        );
    }
}
