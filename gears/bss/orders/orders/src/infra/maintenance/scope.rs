//! D-184 structural scope separation for the bounded maintenance exception (08 §3.5).
//!
//! `DiscoveryScope` is the only holder of `AccessScope::allow_all` in the Orders crates. It is
//! private to this module, builds scoped **selects** only, runs inside a `TxConfig::read_only`
//! transaction and returns owned discovered rows. Writes accept only a [`TargetScope`] built from
//! a discovered row's persisted identifiers and stored properties. Neither type exposes
//! `&AccessScope` to a write path or converts from a raw ID, caller input or `AccessScope`.
//!
//! This file depends only on `super::entity` and toolkit crates so the D-184 compile-fail
//! fixture can include it unchanged.
use super::entity::{
    audit_checkpoint, commercial_attempt, fulfillment_control, gate_outcome, idempotency, order,
    read_access_log, transition_audit,
};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Select};
use std::collections::BTreeSet;
use toolkit_db::Db;
use toolkit_db::secure::{ScopableEntity, Scoped, SecureEntityExt, SecureSelect, TxConfig};
use toolkit_security::access_scope::{AccessScope, ScopeConstraint, ScopeFilter};
use uuid::Uuid;

/// Upper bound of one discovery batch (retention baseline 5,000).
pub const MAX_DISCOVERY_BATCH: u64 = 5000;

/// The closed set of lifecycle-owned maintenance work (08 §3.5). Nothing else may use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceTask {
    StateExpiry,
    DraftAutoVoid,
    IdempotencyCleanup,
    RetentionPurge,
    AuditVerification,
    AuditCheckpoint,
}

/// Configured service actor attributed on worker evidence: never a human, nil UUID, an
/// order's seller or a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceActor {
    subject_id: Uuid,
    subject_tenant_id: Uuid,
}
impl ServiceActor {
    /// `None` for a nil identity: missing internal authority fails the affected job closed.
    pub(crate) fn configured(subject_id: Uuid, subject_tenant_id: Uuid) -> Option<Self> {
        (!subject_id.is_nil() && !subject_tenant_id.is_nil()).then_some(Self {
            subject_id,
            subject_tenant_id,
        })
    }
    pub(crate) fn subject_id(&self) -> Uuid {
        self.subject_id
    }
    pub(crate) fn subject_tenant_id(&self) -> Uuid {
        self.subject_tenant_id
    }
}

/// The bounded internal capability, constructed only by lifecycle-owned worker wiring from
/// explicit configuration. No REST/SDK type accepts it; a PDP outage does not affect it.
#[derive(Debug)]
pub struct MaintenanceAuthority {
    actor: ServiceActor,
    tasks: BTreeSet<MaintenanceTask>,
}
impl MaintenanceAuthority {
    pub(crate) fn configured(
        actor: ServiceActor,
        tasks: impl IntoIterator<Item = MaintenanceTask>,
    ) -> Self {
        Self {
            actor,
            tasks: tasks.into_iter().collect(),
        }
    }
    /// Grant for one allowlisted task, or `None` when that task is not configured.
    pub(crate) fn grant(&self, task: MaintenanceTask) -> Option<TaskGrant<'_>> {
        self.tasks.contains(&task).then_some(TaskGrant {
            actor: self.actor,
            task,
            _authority: std::marker::PhantomData,
        })
    }
}

/// One task's grant; required by every discovery entry point.
#[derive(Debug, Clone, Copy)]
pub struct TaskGrant<'a> {
    actor: ServiceActor,
    task: MaintenanceTask,
    _authority: std::marker::PhantomData<&'a MaintenanceAuthority>,
}
impl TaskGrant<'_> {
    pub(crate) fn actor(&self) -> ServiceActor {
        self.actor
    }
    pub(crate) fn task(&self) -> MaintenanceTask {
        self.task
    }
}

/// Discovery failure.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("maintenance task is not granted this discovery")]
    NotGranted,
    #[error("discovery batch must be 1..=5000")]
    Batch,
    #[error(transparent)]
    Store(#[from] anyhow::Error),
}

/// Read-only discovery authority. Private constructor; select construction only.
pub struct DiscoveryScope(AccessScope);
impl DiscoveryScope {
    // D-184: the only permitted `allow_all` in the Orders crates, confined to read-only
    // discovery of maintenance candidates.
    #[allow(clippy::disallowed_methods)]
    fn new() -> Self {
        Self(AccessScope::allow_all())
    }
    fn select<E: ScopableEntity + EntityTrait>(&self, query: Select<E>) -> SecureSelect<E, Scoped>
    where
        E::Model: Send + Sync,
    {
        query.secure().scope_with(&self.0)
    }
}

fn admit(
    grant: &TaskGrant<'_>,
    allowed: &[MaintenanceTask],
    limit: u64,
) -> Result<(), DiscoveryError> {
    if !allowed.contains(&grant.task) {
        return Err(DiscoveryError::NotGranted);
    }
    if limit == 0 || limit > MAX_DISCOVERY_BATCH {
        return Err(DiscoveryError::Batch);
    }
    Ok(())
}

/// Run one discovery query in a `TxConfig::read_only` transaction that ends before any write.
/// Crate-visible so the read-only rejection is tested on the exact discovery transaction; the
/// closure receives an opaque `DiscoveryScope` it cannot read or convert.
pub async fn read_only<T, F>(db: &Db, query: F) -> Result<T, DiscoveryError>
where
    T: Send + 'static,
    F: for<'t> FnOnce(
            &'t toolkit_db::DbTx<'t>,
            DiscoveryScope,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<T>> + Send + 't>,
        > + Send,
{
    let found = db
        .transaction_ref_mapped_with_config(TxConfig::read_only(), move |tx| {
            query(tx, DiscoveryScope::new())
        })
        .await?;
    Ok(found)
}

/// A due order candidate; owned values only, never a lock or scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredOrder {
    task: MaintenanceTask,
    order_id: Uuid,
    resource_tenant_id: Uuid,
    seller_tenant_id: Uuid,
    payer_tenant_id: Uuid,
    state: String,
    state_entered_at: time::OffsetDateTime,
    current_version: i32,
}
impl DiscoveredOrder {
    pub(crate) fn order_id(&self) -> Uuid {
        self.order_id
    }
    pub(crate) fn task(&self) -> MaintenanceTask {
        self.task
    }
    /// Whether the locked row still has the discovered persisted facts.
    pub(crate) fn unchanged(&self, row: &order::Model) -> bool {
        row.order_id == self.order_id
            && row.resource_tenant_id == self.resource_tenant_id
            && row.seller_tenant_id == self.seller_tenant_id
            && row.payer_tenant_id == self.payer_tenant_id
            && row.state == self.state
            && row.state_entered_at == self.state_entered_at
            && row.current_version == self.current_version
    }
}

/// Discover orders in `state` that entered it before `entered_before` (expiry/auto-void).
///
/// # Errors
/// Ungranted task, invalid batch or store failure.
pub async fn discover_orders(
    db: &Db,
    grant: &TaskGrant<'_>,
    state: &str,
    entered_before: time::OffsetDateTime,
    limit: u64,
) -> Result<Vec<DiscoveredOrder>, DiscoveryError> {
    admit(
        grant,
        &[MaintenanceTask::StateExpiry, MaintenanceTask::DraftAutoVoid],
        limit,
    )?;
    let task = grant.task;
    let state = state.to_owned();
    read_only(db, move |tx, discovery| {
        Box::pin(async move {
            let rows = discovery
                .select(
                    order::Entity::find()
                        .filter(order::Column::State.eq(state))
                        .filter(order::Column::StateEnteredAt.lt(entered_before))
                        .order_by_asc(order::Column::StateEnteredAt)
                        .order_by_asc(order::Column::OrderId)
                        .limit(limit),
                )
                .all(tx)
                .await?;
            Ok(rows
                .into_iter()
                .map(|row| DiscoveredOrder {
                    task,
                    order_id: row.order_id,
                    resource_tenant_id: row.resource_tenant_id,
                    seller_tenant_id: row.seller_tenant_id,
                    payer_tenant_id: row.payer_tenant_id,
                    state: row.state,
                    state_entered_at: row.state_entered_at,
                    current_version: row.current_version,
                })
                .collect())
        })
    })
    .await
}

/// An expired registry marker candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredIdempotency(idempotency::Model);
impl DiscoveredIdempotency {
    /// The keyset position of this marker in the cleanup worker's `(expires_at, execution_id)`
    /// order, so the next pass can continue behind it.
    #[must_use]
    pub fn cursor(&self) -> MarkerCursor {
        MarkerCursor {
            expires_at: self.0.expires_at,
            execution_id: self.0.execution_id,
        }
    }
}

/// A keyset position in the expired-marker order `(expires_at, execution_id)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkerCursor {
    pub expires_at: time::OffsetDateTime,
    pub execution_id: Uuid,
}

/// Discover registry markers whose response window has expired, oldest expiry first.
///
/// # Errors
/// Ungranted task, invalid batch or store failure.
pub async fn discover_expired_idempotency(
    db: &Db,
    grant: &TaskGrant<'_>,
    now: time::OffsetDateTime,
    limit: u64,
) -> Result<Vec<DiscoveredIdempotency>, DiscoveryError> {
    discover_expired_idempotency_after(db, grant, now, None, limit).await
}

/// Discover expired registry markers strictly after the keyset position `after` in
/// `(expires_at, execution_id)` order. The cleanup worker carries the position across passes so
/// that markers it must preserve (live leases, unresolved executions) cannot occupy every slot of
/// every bounded pass and starve the deletable markers behind them.
///
/// # Errors
/// Ungranted task, invalid batch or store failure.
pub async fn discover_expired_idempotency_after(
    db: &Db,
    grant: &TaskGrant<'_>,
    now: time::OffsetDateTime,
    after: Option<MarkerCursor>,
    limit: u64,
) -> Result<Vec<DiscoveredIdempotency>, DiscoveryError> {
    admit(grant, &[MaintenanceTask::IdempotencyCleanup], limit)?;
    read_only(db, move |tx, discovery| {
        Box::pin(async move {
            let mut query =
                idempotency::Entity::find().filter(idempotency::Column::ExpiresAt.lt(now));
            if let Some(after) = after {
                query = query.filter(
                    Condition::any()
                        .add(idempotency::Column::ExpiresAt.gt(after.expires_at))
                        .add(
                            Condition::all()
                                .add(idempotency::Column::ExpiresAt.eq(after.expires_at))
                                .add(idempotency::Column::ExecutionId.gt(after.execution_id)),
                        ),
                );
            }
            let rows = discovery
                .select(
                    query
                        .order_by_asc(idempotency::Column::ExpiresAt)
                        .order_by_asc(idempotency::Column::ExecutionId)
                        .limit(limit),
                )
                .all(tx)
                .await?;
            Ok(rows.into_iter().map(DiscoveredIdempotency).collect())
        })
    })
    .await
}

/// Retention-eligible candidate IDs of one table, plus the namespace their grant names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRetention {
    table: RetentionTable,
    ids: Vec<Uuid>,
    tenants: Vec<Uuid>,
    cutoff: time::OffsetDateTime,
}

/// The three bounded retention stores (D-49/D-100).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionTable {
    RefusedAudit,
    PreviewDiagnostics,
    ReadAccessLog,
}

/// Discover expired rows of one retention store before `cutoff`.
///
/// # Errors
/// Ungranted task, invalid batch or store failure.
pub async fn discover_retention(
    db: &Db,
    grant: &TaskGrant<'_>,
    table: RetentionTable,
    cutoff: time::OffsetDateTime,
    limit: u64,
) -> Result<DiscoveredRetention, DiscoveryError> {
    admit(grant, &[MaintenanceTask::RetentionPurge], limit)?;
    read_only(db, move |tx, discovery| {
        Box::pin(async move {
            let (ids, tenants): (Vec<Uuid>, Vec<Uuid>) = match table {
                RetentionTable::RefusedAudit => discovery
                    .select(
                        transition_audit::Entity::find()
                            .filter(transition_audit::Column::Outcome.eq("refused"))
                            .filter(transition_audit::Column::CreatedAt.lt(cutoff))
                            .order_by_asc(transition_audit::Column::CreatedAt)
                            .order_by_asc(transition_audit::Column::AuditId)
                            .limit(limit),
                    )
                    .all(tx)
                    .await?
                    .into_iter()
                    .map(|r| (r.audit_id, r.subject_tenant_id))
                    .unzip(),
                RetentionTable::PreviewDiagnostics => discovery
                    .select(
                        gate_outcome::Entity::find()
                            .filter(gate_outcome::Column::OrderId.is_null())
                            .filter(gate_outcome::Column::EvaluatedAt.lt(cutoff))
                            .order_by_asc(gate_outcome::Column::EvaluatedAt)
                            .order_by_asc(gate_outcome::Column::OutcomeId)
                            .limit(limit),
                    )
                    .all(tx)
                    .await?
                    .into_iter()
                    .map(|r| (r.outcome_id, r.subject_tenant_id))
                    .unzip(),
                RetentionTable::ReadAccessLog => discovery
                    .select(
                        read_access_log::Entity::find()
                            .filter(read_access_log::Column::AccessedAt.lt(cutoff))
                            .order_by_asc(read_access_log::Column::AccessedAt)
                            .order_by_asc(read_access_log::Column::AccessId)
                            .limit(limit),
                    )
                    .all(tx)
                    .await?
                    .into_iter()
                    .map(|r| (r.access_id, r.access_id))
                    .unzip(),
            };
            let mut tenants = tenants;
            tenants.sort_unstable();
            tenants.dedup();
            Ok(DiscoveredRetention {
                table,
                ids,
                tenants,
                cutoff,
            })
        })
    })
    .await
}

/// Eligible rows still present after a purge pass (01 §3.5 item 4): count and oldest instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionBacklog {
    pub rows: u64,
    pub oldest: Option<time::OffsetDateTime>,
}

/// Count expired rows of one retention store before `cutoff` and find the oldest.
///
/// # Errors
/// Ungranted task or store failure.
pub async fn discover_retention_backlog(
    db: &Db,
    grant: &TaskGrant<'_>,
    table: RetentionTable,
    cutoff: time::OffsetDateTime,
) -> Result<RetentionBacklog, DiscoveryError> {
    admit(grant, &[MaintenanceTask::RetentionPurge], 1)?;
    read_only(db, move |tx, discovery| {
        Box::pin(async move {
            let (rows, oldest) = match table {
                RetentionTable::RefusedAudit => {
                    let query = transition_audit::Entity::find()
                        .filter(transition_audit::Column::Outcome.eq("refused"))
                        .filter(transition_audit::Column::CreatedAt.lt(cutoff));
                    let rows = discovery.select(query.clone()).count(tx).await?;
                    let oldest = discovery
                        .select(
                            query
                                .order_by_asc(transition_audit::Column::CreatedAt)
                                .limit(1),
                        )
                        .one(tx)
                        .await?
                        .map(|r| r.created_at);
                    (rows, oldest)
                }
                RetentionTable::PreviewDiagnostics => {
                    let query = gate_outcome::Entity::find()
                        .filter(gate_outcome::Column::OrderId.is_null())
                        .filter(gate_outcome::Column::EvaluatedAt.lt(cutoff));
                    let rows = discovery.select(query.clone()).count(tx).await?;
                    let oldest = discovery
                        .select(
                            query
                                .order_by_asc(gate_outcome::Column::EvaluatedAt)
                                .limit(1),
                        )
                        .one(tx)
                        .await?
                        .map(|r| r.evaluated_at);
                    (rows, oldest)
                }
                RetentionTable::ReadAccessLog => {
                    let query = read_access_log::Entity::find()
                        .filter(read_access_log::Column::AccessedAt.lt(cutoff));
                    let rows = discovery.select(query.clone()).count(tx).await?;
                    let oldest = discovery
                        .select(
                            query
                                .order_by_asc(read_access_log::Column::AccessedAt)
                                .limit(1),
                        )
                        .one(tx)
                        .await?
                        .map(|r| r.accessed_at);
                    (rows, oldest)
                }
            };
            Ok(RetentionBacklog { rows, oldest })
        })
    })
    .await
}

/// Expired markers still present after a cleanup pass: count and oldest expiry.
///
/// # Errors
/// Ungranted task or store failure.
pub async fn discover_idempotency_backlog(
    db: &Db,
    grant: &TaskGrant<'_>,
    now: time::OffsetDateTime,
) -> Result<RetentionBacklog, DiscoveryError> {
    admit(grant, &[MaintenanceTask::IdempotencyCleanup], 1)?;
    read_only(db, move |tx, discovery| {
        Box::pin(async move {
            let query = idempotency::Entity::find().filter(idempotency::Column::ExpiresAt.lt(now));
            let rows = discovery.select(query.clone()).count(tx).await?;
            let oldest = discovery
                .select(query.order_by_asc(idempotency::Column::ExpiresAt).limit(1))
                .one(tx)
                .await?
                .map(|r| r.expires_at);
            Ok(RetentionBacklog { rows, oldest })
        })
    })
    .await
}

/// Which durable execution table an unresolved candidate lives in (D-188 / D-198).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionKind {
    CommercialAttempt,
    FulfillmentControl,
}

/// An unresolved durable execution whose owner lease lapsed: owned persisted facts for the
/// bounded recovery continuation (DESIGN D-188 "Recovery is an engine service ... bounded
/// continuation in the existing idempotency maintenance worker"). The worker never deletes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredExecution {
    kind: ExecutionKind,
    id: Uuid,
    order_id: Uuid,
    execution_id: Uuid,
    status: String,
    owner_token: Uuid,
    fencing_generation: i64,
    lease_until: Option<time::OffsetDateTime>,
    created_at: time::OffsetDateTime,
}
impl DiscoveredExecution {
    #[must_use]
    pub fn kind(&self) -> ExecutionKind {
        self.kind
    }
    /// The attempt or control ID.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }
    #[must_use]
    pub fn order_id(&self) -> Uuid {
        self.order_id
    }
    #[must_use]
    pub fn execution_id(&self) -> Uuid {
        self.execution_id
    }
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }
    #[must_use]
    pub fn owner_token(&self) -> Uuid {
        self.owner_token
    }
    #[must_use]
    pub fn fencing_generation(&self) -> i64 {
        self.fencing_generation
    }
    #[must_use]
    pub fn lease_until(&self) -> Option<time::OffsetDateTime> {
        self.lease_until
    }
    #[must_use]
    pub fn created_at(&self) -> time::OffsetDateTime {
        self.created_at
    }
    /// The keyset position of this execution in the continuation's `(lease_until, id)` order.
    #[must_use]
    pub fn cursor(&self) -> ExecutionCursor {
        ExecutionCursor {
            lease_until: self.lease_until,
            id: self.id,
        }
    }
}

/// A keyset position in the lapsed-execution order `(lease_until, id)`; a lapsed execution
/// always carries a lease instant, so `None` sorts first and is never returned by discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionCursor {
    pub lease_until: Option<time::OffsetDateTime>,
    pub id: Uuid,
}

/// The lapsed unresolved executions of one discovery: the bounded page after the cursor, plus
/// the whole-set backlog figures (count and oldest lease) so the D-188 backlog/oldest-age
/// alerts describe the entire set, not the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LapsedExecutions {
    pub found: Vec<DiscoveredExecution>,
    pub total: u64,
    pub oldest_lease: Option<time::OffsetDateTime>,
}

/// Unresolved executions (attempt `prepared`/`running`, control `prepared`/`awaiting`/
/// `barrier_ready`) whose lease lapsed before `now`, oldest lease first and strictly after the
/// keyset position `after`, plus the total count and oldest lease of such executions so the
/// backlog is reported even beyond the batch. The continuation carries the position across
/// passes so a set of executions it cannot progress cannot starve the ones behind them.
///
/// # Errors
/// Ungranted task, invalid batch or store failure.
pub async fn discover_expired_executions(
    db: &Db,
    grant: &TaskGrant<'_>,
    now: time::OffsetDateTime,
    after: Option<ExecutionCursor>,
    limit: u64,
) -> Result<LapsedExecutions, DiscoveryError> {
    admit(grant, &[MaintenanceTask::IdempotencyCleanup], limit)?;
    read_only(db, move |tx, discovery| {
        Box::pin(async move {
            let mut attempts = commercial_attempt::Entity::find()
                .filter(commercial_attempt::Column::Status.is_in(["prepared", "running"]))
                .filter(commercial_attempt::Column::LeaseUntil.lt(now));
            let mut controls = fulfillment_control::Entity::find()
                .filter(fulfillment_control::Column::Status.is_in([
                    "prepared",
                    "awaiting",
                    "barrier_ready",
                ]))
                .filter(fulfillment_control::Column::LeaseUntil.lt(now));
            let total = discovery.select(attempts.clone()).count(tx).await?
                + discovery.select(controls.clone()).count(tx).await?;
            let oldest_attempt = discovery
                .select(
                    attempts
                        .clone()
                        .order_by_asc(commercial_attempt::Column::LeaseUntil)
                        .limit(1),
                )
                .one(tx)
                .await?
                .and_then(|a| a.lease_until);
            let oldest_control = discovery
                .select(
                    controls
                        .clone()
                        .order_by_asc(fulfillment_control::Column::LeaseUntil)
                        .limit(1),
                )
                .one(tx)
                .await?
                .and_then(|c| c.lease_until);
            let oldest_lease = match (oldest_attempt, oldest_control) {
                (Some(a), Some(c)) => Some(a.min(c)),
                (a, c) => a.or(c),
            };
            if let Some(ExecutionCursor {
                lease_until: Some(lease_until),
                id,
            }) = after
            {
                attempts = attempts.filter(
                    Condition::any()
                        .add(commercial_attempt::Column::LeaseUntil.gt(lease_until))
                        .add(
                            Condition::all()
                                .add(commercial_attempt::Column::LeaseUntil.eq(lease_until))
                                .add(commercial_attempt::Column::AttemptId.gt(id)),
                        ),
                );
                controls = controls.filter(
                    Condition::any()
                        .add(fulfillment_control::Column::LeaseUntil.gt(lease_until))
                        .add(
                            Condition::all()
                                .add(fulfillment_control::Column::LeaseUntil.eq(lease_until))
                                .add(fulfillment_control::Column::ControlId.gt(id)),
                        ),
                );
            }
            let mut found: Vec<DiscoveredExecution> = discovery
                .select(
                    attempts
                        .order_by_asc(commercial_attempt::Column::LeaseUntil)
                        .order_by_asc(commercial_attempt::Column::AttemptId)
                        .limit(limit),
                )
                .all(tx)
                .await?
                .into_iter()
                .map(|a| DiscoveredExecution {
                    kind: ExecutionKind::CommercialAttempt,
                    id: a.attempt_id,
                    order_id: a.order_id,
                    execution_id: a.idempotency_execution_id,
                    status: a.status,
                    owner_token: a.owner_token,
                    fencing_generation: a.fencing_generation,
                    lease_until: a.lease_until,
                    created_at: a.created_at,
                })
                .collect();
            found.extend(
                discovery
                    .select(
                        controls
                            .order_by_asc(fulfillment_control::Column::LeaseUntil)
                            .order_by_asc(fulfillment_control::Column::ControlId)
                            .limit(limit),
                    )
                    .all(tx)
                    .await?
                    .into_iter()
                    .map(|c| DiscoveredExecution {
                        kind: ExecutionKind::FulfillmentControl,
                        id: c.control_id,
                        order_id: c.order_id,
                        execution_id: c.idempotency_execution_id,
                        status: c.status,
                        owner_token: c.owner_token,
                        fencing_generation: c.fencing_generation,
                        lease_until: c.lease_until,
                        created_at: c.created_at,
                    }),
            );
            found.sort_by(|a, b| a.lease_until.cmp(&b.lease_until).then(a.id.cmp(&b.id)));
            found.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
            Ok(LapsedExecutions {
                found,
                total,
                oldest_lease,
            })
        })
    })
    .await
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct ClockRow {
    observed_at: time::OffsetDateTime,
}

/// Fresh database wall-clock time (`clock_timestamp()`) read through the discovery role of the
/// granted task, so every worker cutoff comes from the database rather than a replica clock.
/// The aggregate form returns one row even when the probed table is empty.
///
/// # Errors
/// Ungranted task or store failure.
pub async fn discover_clock(
    db: &Db,
    grant: &TaskGrant<'_>,
) -> Result<time::OffsetDateTime, DiscoveryError> {
    let task = grant.task();
    read_only(db, move |tx, discovery| {
        Box::pin(async move {
            // The retention role reads evidence stores only; every other class reads the
            // aggregate. Both probes carry the discovery scope and return exactly one row.
            let rows = if task == MaintenanceTask::RetentionPurge {
                discovery
                    .select(transition_audit::Entity::find())
                    .project_all(tx, |q| {
                        q.select_only()
                            .column_as(Expr::cust("clock_timestamp()"), "observed_at")
                            .column_as(Expr::cust("count(*)"), "n")
                            .into_model::<ClockRow>()
                    })
                    .await?
            } else {
                discovery
                    .select(order::Entity::find())
                    .project_all(tx, |q| {
                        q.select_only()
                            .column_as(Expr::cust("clock_timestamp()"), "observed_at")
                            .column_as(Expr::cust("count(*)"), "n")
                            .into_model::<ClockRow>()
                    })
                    .await?
            };
            rows.first()
                .map(|r| r.observed_at)
                .ok_or_else(|| anyhow::anyhow!("database clock unavailable"))
        })
    })
    .await
}

/// An immutable audit namespace the audit worker must visit: it holds a live order, a recorded
/// checkpoint or a committed audit row (verifier/checkpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveredAuditNamespace {
    audit_tenant_id: Uuid,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct NamespaceRow {
    audit_tenant_id: Uuid,
}

/// Discover audit namespaces: up to `limit` **distinct** namespaces after the `after` keyset
/// cursor, so one namespace with many rows never hides the others.
///
/// D-100 item 2 enumerates **orders, not only surviving audit rows**, "so an empty/deleted
/// trail is a finding", and every order recorded by a previous checkpoint. The roster is
/// therefore the union of three sources, each keyset-paged on its own index: the immutable
/// `audit_tenant_id` of every live order (any counter: an empty tenant is checkpointed with
/// no members, a counter rolled back to zero above surviving rows is an overflow finding),
/// of every checkpoint header, and of every committed audit row. A namespace whose whole
/// committed trail was removed, or whose only order was removed after a checkpoint recorded
/// it, stays scheduled, so its checkpoint phase reports the missing heads or orders and its
/// verifier the incomplete chains instead of the namespace silently vanishing from the
/// schedule.
///
/// # Errors
/// Ungranted task, invalid batch or store failure.
pub async fn discover_audit_namespaces(
    db: &Db,
    grant: &TaskGrant<'_>,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<DiscoveredAuditNamespace>, DiscoveryError> {
    admit(
        grant,
        &[
            MaintenanceTask::AuditVerification,
            MaintenanceTask::AuditCheckpoint,
        ],
        limit,
    )?;
    read_only(db, move |tx, discovery| {
        Box::pin(async move {
            let mut orders = order::Entity::find();
            let mut checkpoints = audit_checkpoint::Entity::find();
            let mut committed = transition_audit::Entity::find()
                .filter(transition_audit::Column::Outcome.eq("committed"))
                .filter(transition_audit::Column::AuditTenantId.is_not_null());
            if let Some(after) = after {
                orders = orders.filter(order::Column::AuditTenantId.gt(after));
                checkpoints = checkpoints.filter(audit_checkpoint::Column::AuditTenantId.gt(after));
                committed = committed.filter(transition_audit::Column::AuditTenantId.gt(after));
            }
            let from_orders = discovery
                .select(orders)
                .project_all(tx, |q| {
                    q.select_only()
                        .column(order::Column::AuditTenantId)
                        .distinct()
                        .order_by_asc(order::Column::AuditTenantId)
                        .limit(limit)
                        .into_model::<NamespaceRow>()
                })
                .await?;
            let from_checkpoints = discovery
                .select(checkpoints)
                .project_all(tx, |q| {
                    q.select_only()
                        .column(audit_checkpoint::Column::AuditTenantId)
                        .distinct()
                        .order_by_asc(audit_checkpoint::Column::AuditTenantId)
                        .limit(limit)
                        .into_model::<NamespaceRow>()
                })
                .await?;
            let from_committed = discovery
                .select(committed)
                .project_all(tx, |q| {
                    q.select_only()
                        .column(transition_audit::Column::AuditTenantId)
                        .distinct()
                        .order_by_asc(transition_audit::Column::AuditTenantId)
                        .limit(limit)
                        .into_model::<NamespaceRow>()
                })
                .await?;
            // Each source holds its first `limit` namespaces after the cursor, so the first
            // `limit` of their ordered union is exact for the combined roster.
            let merged = from_orders
                .into_iter()
                .chain(from_checkpoints)
                .chain(from_committed)
                .map(|r| r.audit_tenant_id)
                .collect::<BTreeSet<_>>();
            Ok(merged
                .into_iter()
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .map(|audit_tenant_id| DiscoveredAuditNamespace { audit_tenant_id })
                .collect())
        })
    })
    .await
}

#[derive(Debug, Clone, PartialEq)]
enum Target {
    Order(DiscoveredOrder),
    Idempotency(Box<idempotency::Model>),
    Retention(DiscoveredRetention),
    AuditNamespace(Uuid),
}

/// Write authority narrowed to discovered persisted targets. No raw constructor exists.
#[derive(Debug, Clone)]
pub struct TargetScope {
    scope: AccessScope,
    target: Target,
}
impl TargetScope {
    /// Standard order ID conjoined with the row's stored resource/seller/payer properties.
    #[must_use]
    pub fn from_discovered_order(row: &DiscoveredOrder) -> Self {
        Self {
            scope: AccessScope::single(ScopeConstraint::new(vec![
                ScopeFilter::eq(toolkit_security::pep_properties::RESOURCE_ID, row.order_id),
                ScopeFilter::eq("resource_tenant_id", row.resource_tenant_id),
                ScopeFilter::eq("seller_tenant_id", row.seller_tenant_id),
                ScopeFilter::eq("payer_tenant_id", row.payer_tenant_id),
            ])),
            target: Target::Order(row.clone()),
        }
    }
    /// The marker's immutable execution ID and its principal/operation/key.
    #[must_use]
    pub fn from_discovered_idempotency(row: &DiscoveredIdempotency) -> Self {
        let row = &row.0;
        Self {
            scope: AccessScope::single(ScopeConstraint::new(vec![
                ScopeFilter::eq(
                    toolkit_security::pep_properties::RESOURCE_ID,
                    row.execution_id,
                ),
                ScopeFilter::eq("principal_scope", row.principal_scope.clone()),
                ScopeFilter::eq("operation", row.operation.clone()),
                ScopeFilter::eq("idempotency_key", row.idempotency_key.clone()),
            ])),
            target: Target::Idempotency(Box::new(row.clone())),
        }
    }
    /// The discovered IDs' table-specific restriction (subject tenant or record ID).
    #[must_use]
    pub fn from_discovered_retention(batch: &DiscoveredRetention) -> Self {
        let property = match batch.table {
            RetentionTable::RefusedAudit => "subject_tenant_id",
            RetentionTable::PreviewDiagnostics => toolkit_security::pep_properties::OWNER_TENANT_ID,
            RetentionTable::ReadAccessLog => toolkit_security::pep_properties::RESOURCE_ID,
        };
        let scope = if batch.tenants.is_empty() {
            AccessScope::deny_all()
        } else {
            AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
                property,
                batch.tenants.clone(),
            )]))
        };
        Self {
            scope,
            target: Target::Retention(batch.clone()),
        }
    }
    /// The immutable audit namespace (checkpoint/verifier tables use it as `tenant_col`).
    #[must_use]
    pub fn from_discovered_audit_namespace(row: &DiscoveredAuditNamespace) -> Self {
        Self {
            scope: AccessScope::for_tenant(row.audit_tenant_id),
            target: Target::AuditNamespace(row.audit_tenant_id),
        }
    }

    pub(crate) fn access_scope(&self) -> &AccessScope {
        &self.scope
    }
    pub(crate) fn order(&self) -> Option<&DiscoveredOrder> {
        match &self.target {
            Target::Order(row) => Some(row),
            _ => None,
        }
    }
    pub(crate) fn idempotency(&self) -> Option<&idempotency::Model> {
        match &self.target {
            Target::Idempotency(row) => Some(row.as_ref()),
            _ => None,
        }
    }
    /// The retention batch of exactly `table`, with its discovery cutoff.
    pub(crate) fn retention(
        &self,
        table: RetentionTable,
    ) -> Option<(&[Uuid], time::OffsetDateTime)> {
        match &self.target {
            Target::Retention(batch) if batch.table == table => {
                Some((batch.ids.as_slice(), batch.cutoff))
            }
            _ => None,
        }
    }
    pub(crate) fn audit_namespace(&self) -> Option<Uuid> {
        match &self.target {
            Target::AuditNamespace(id) => Some(*id),
            _ => None,
        }
    }
    /// The discovered namespace as a scope over the aggregate and audit stores, whose scope
    /// property is the immutable `audit_tenant_id` (never today's resource tenant). `None` for
    /// a non-namespace target.
    pub(crate) fn namespace_evidence_scope(&self) -> Option<AccessScope> {
        self.audit_namespace().map(|namespace| {
            AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::eq(
                "audit_tenant_id",
                namespace,
            )]))
        })
    }
}
