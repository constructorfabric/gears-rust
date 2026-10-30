//! D-184 structural scope separation for the bounded maintenance exception (08 §3.5).
//!
//! `DiscoveryScope` is the only holder of `AccessScope::allow_all` in the Orders crates. It is
//! private to this module, builds scoped **selects** only, runs inside a `TxConfig::read_only`
//! transaction and returns owned discovered rows. Writes accept only a [`TargetScope`] built from
//! a discovered row's persisted identifiers and stored properties. Neither type exposes
//! `&AccessScope` to a write path or converts from a raw ID, caller input or `AccessScope`.
//!
//! The discovery statements themselves live in `infra::storage::repo::discovery`; this module
//! keeps the grant, the batch bound, the read-only transaction and the scope, and hands those
//! statements only a private select-scoping adapter ([`Reads`]).
//!
//! This file depends only on `super::{entity, discovery, MaintenanceTask}` and toolkit crates so
//! the D-184 compile-fail fixture can include it unchanged.
use super::MaintenanceTask;
use super::discovery::{self, ClockTable, DiscoveryReads};
use super::entity::{idempotency, order};
use sea_orm::{EntityTrait, Select};
use std::collections::BTreeSet;
use toolkit_db::Db;
use toolkit_db::secure::{ScopableEntity, Scoped, SecureEntityExt, SecureSelect, TxConfig};
use toolkit_security::access_scope::{AccessScope, ScopeConstraint, ScopeFilter};
use uuid::Uuid;

/// Upper bound of one discovery batch (retention baseline 5,000).
pub const MAX_DISCOVERY_BATCH: u64 = 5000;

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
    /// The actor's subject ID.
    pub(crate) fn subject_id(&self) -> Uuid {
        self.subject_id
    }
    /// The actor's subject tenant ID.
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
    /// The authority for `actor` over exactly the configured `tasks`.
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
    /// The service actor the grant attributes work to.
    pub(crate) fn actor(&self) -> ServiceActor {
        self.actor
    }
    /// The granted task.
    pub(crate) fn task(&self) -> MaintenanceTask {
        self.task
    }
}

/// Discovery failure.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// The grant's task may not run this discovery.
    #[error("maintenance task is not granted this discovery")]
    NotGranted,
    /// The batch limit is outside `1..=MAX_DISCOVERY_BATCH`.
    #[error("discovery batch must be 1..=5000")]
    Batch,
    /// The discovery query failed.
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

/// The discovery scope as the storage discovery statements see it: it scopes a select and
/// nothing else. Private, so only this module's entry points can lend it to those statements.
struct Reads<'a>(&'a DiscoveryScope);
impl DiscoveryReads for Reads<'_> {
    fn scoped<E>(&self, query: Select<E>) -> SecureSelect<E, Scoped>
    where
        E: ScopableEntity + EntityTrait,
        E::Model: Send + Sync,
    {
        self.0.select(query)
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
    /// The discovered order.
    pub(crate) fn order_id(&self) -> Uuid {
        self.order_id
    }
    /// The task the order was discovered for.
    pub(crate) fn task(&self) -> MaintenanceTask {
        self.task
    }
    /// Whether the locked row still has the discovered persisted facts.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "only `lock_target_order` rechecks with it, whose callers are Stage 5 sweeps"
        )
    )]
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
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "expiry and auto-void (Stage 5) are its production callers; the maintenance tests \
                  exercise it now"
    )
)]
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
            let rows =
                discovery::due_orders(tx, &Reads(&discovery), state, entered_before, limit).await?;
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
    /// The marker's execution generation (a server identity, safe to log).
    #[must_use]
    pub fn execution_id(&self) -> Uuid {
        self.0.execution_id
    }
    /// The operation the marker's key scopes (a catalog identifier, safe to log).
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.0.operation
    }
    /// The target order; `None` for an in-flight or refused create.
    #[must_use]
    pub fn order_id(&self) -> Option<Uuid> {
        self.0.order_id
    }
}

/// A keyset position in the expired-marker order `(expires_at, execution_id)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkerCursor {
    /// The marker's window expiry.
    pub expires_at: time::OffsetDateTime,
    /// The marker's execution ID (tie-breaker).
    pub execution_id: Uuid,
}

/// Discover registry markers whose response window has expired, oldest expiry first.
///
/// # Errors
/// Ungranted task, invalid batch or store failure.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the idempotency cleanup pass discovers through the registry repository; the \
                  idempotency tests prove this discovery path"
    )
)]
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
    let after = after.map(|c| (c.expires_at, c.execution_id));
    read_only(db, move |tx, discovery| {
        Box::pin(async move {
            let rows =
                discovery::expired_markers(tx, &Reads(&discovery), now, after, limit).await?;
            Ok(rows.into_iter().map(DiscoveredIdempotency).collect())
        })
    })
    .await
}

/// Retention-eligible candidate IDs of one table, plus the values its purge scope filters on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRetention {
    table: RetentionTable,
    ids: Vec<Uuid>,
    /// Sorted, deduplicated values of the table's scope property ([`TargetScope::
    /// from_discovered_retention`]), per table:
    /// - [`RetentionTable::RefusedAudit`]: the rows' `subject_tenant_id`s, filtered as
    ///   `subject_tenant_id`;
    /// - [`RetentionTable::PreviewDiagnostics`]: the rows' `subject_tenant_id`s, filtered as the
    ///   owner tenant;
    /// - [`RetentionTable::ReadAccessLog`]: the rows' own `access_id`s (the log has no tenant
    ///   column to narrow by), filtered as the resource ID.
    scope_values: Vec<Uuid>,
    cutoff: time::OffsetDateTime,
}

/// The three bounded retention stores (D-49/D-100).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionTable {
    /// Refused transition audit rows.
    RefusedAudit,
    /// Gate outcomes of previews (no order attached).
    PreviewDiagnostics,
    /// The read access log.
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
            let reads = Reads(&discovery);
            let (ids, mut scope_values): (Vec<Uuid>, Vec<Uuid>) = match table {
                RetentionTable::RefusedAudit => {
                    discovery::refused_audit_candidates(tx, &reads, cutoff, limit)
                        .await?
                        .into_iter()
                        .unzip()
                }
                RetentionTable::PreviewDiagnostics => {
                    discovery::preview_diagnostic_candidates(tx, &reads, cutoff, limit)
                        .await?
                        .into_iter()
                        .unzip()
                }
                RetentionTable::ReadAccessLog => {
                    let ids = discovery::access_log_candidates(tx, &reads, cutoff, limit).await?;
                    (ids.clone(), ids)
                }
            };
            scope_values.sort_unstable();
            scope_values.dedup();
            Ok(DiscoveredRetention {
                table,
                ids,
                scope_values,
                cutoff,
            })
        })
    })
    .await
}

/// Eligible rows still present after a purge pass (01 §3.5 item 4): count and oldest instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionBacklog {
    /// Eligible rows still present.
    pub rows: u64,
    /// The timestamp of the oldest eligible row, if any.
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
            let reads = Reads(&discovery);
            let (rows, oldest) = match table {
                RetentionTable::RefusedAudit => {
                    discovery::refused_audit_backlog(tx, &reads, cutoff).await?
                }
                RetentionTable::PreviewDiagnostics => {
                    discovery::preview_diagnostics_backlog(tx, &reads, cutoff).await?
                }
                RetentionTable::ReadAccessLog => {
                    discovery::access_log_backlog(tx, &reads, cutoff).await?
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
            let (rows, oldest) = discovery::marker_backlog(tx, &Reads(&discovery), now).await?;
            Ok(RetentionBacklog { rows, oldest })
        })
    })
    .await
}

/// Which durable execution table an unresolved candidate lives in (D-188 / D-198).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionKind {
    /// A commercial attempt.
    CommercialAttempt,
    /// A fulfillment control.
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
#[expect(
    dead_code,
    reason = "the D-188/D-198 continuation bodies that read these facts are Stage 5 ports; no \
              caller reads them yet, tests included"
)]
impl DiscoveredExecution {
    /// The table the execution lives in.
    #[must_use]
    pub fn kind(&self) -> ExecutionKind {
        self.kind
    }
    /// The attempt or control ID.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }
    /// The order the execution belongs to.
    #[must_use]
    pub fn order_id(&self) -> Uuid {
        self.order_id
    }
    /// The idempotency execution ID the execution is linked to.
    #[must_use]
    pub fn execution_id(&self) -> Uuid {
        self.execution_id
    }
    /// The persisted unresolved status.
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }
    /// The owner token of the lapsed lease.
    #[must_use]
    pub fn owner_token(&self) -> Uuid {
        self.owner_token
    }
    /// The fencing generation at discovery.
    #[must_use]
    pub fn fencing_generation(&self) -> i64 {
        self.fencing_generation
    }
    /// The owner lease expiry.
    #[must_use]
    pub fn lease_until(&self) -> Option<time::OffsetDateTime> {
        self.lease_until
    }
    /// When the execution was created.
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
    /// The lease instant.
    pub lease_until: Option<time::OffsetDateTime>,
    /// The attempt or control ID (tie-breaker).
    pub id: Uuid,
}

/// The lapsed unresolved executions of one discovery: the bounded page after the cursor, plus
/// the whole-set backlog figures (count and oldest lease) so the D-188 backlog/oldest-age
/// alerts describe the entire set, not the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LapsedExecutions {
    /// The bounded page after the cursor.
    pub found: Vec<DiscoveredExecution>,
    /// Lapsed unresolved executions in the whole set.
    pub total: u64,
    /// The oldest lapsed lease in the whole set.
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
    // A lapsed execution always carries a lease instant; a cursor without one restarts.
    let after = after.and_then(|c| c.lease_until.map(|lease_until| (lease_until, c.id)));
    read_only(db, move |tx, discovery| {
        Box::pin(async move {
            let lapsed =
                discovery::lapsed_executions(tx, &Reads(&discovery), now, after, limit).await?;
            let mut found: Vec<DiscoveredExecution> = lapsed
                .attempts
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
            found.extend(lapsed.controls.into_iter().map(|c| DiscoveredExecution {
                kind: ExecutionKind::FulfillmentControl,
                id: c.control_id,
                order_id: c.order_id,
                execution_id: c.idempotency_execution_id,
                status: c.status,
                owner_token: c.owner_token,
                fencing_generation: c.fencing_generation,
                lease_until: c.lease_until,
                created_at: c.created_at,
            }));
            found.sort_by(|a, b| a.lease_until.cmp(&b.lease_until).then(a.id.cmp(&b.id)));
            found.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
            Ok(LapsedExecutions {
                found,
                total: lapsed.total,
                oldest_lease: lapsed.oldest_lease,
            })
        })
    })
    .await
}

/// Fresh database wall-clock time (`clock_timestamp()`) read through the discovery role of the
/// granted task, so every worker cutoff comes from the database rather than a replica clock.
/// The aggregate form returns one row even when the probed table is empty, and the constant
/// false filter keeps PostgreSQL from scanning the table for a count nobody reads.
///
/// # Errors
/// Ungranted task or store failure.
pub async fn discover_clock(
    db: &Db,
    grant: &TaskGrant<'_>,
) -> Result<time::OffsetDateTime, DiscoveryError> {
    // The retention role reads evidence stores only; every other class reads the aggregate.
    let table = if grant.task() == MaintenanceTask::RetentionPurge {
        ClockTable::Evidence
    } else {
        ClockTable::Aggregate
    };
    read_only(db, move |tx, discovery| {
        Box::pin(async move { discovery::clock(tx, &Reads(&discovery), table).await })
    })
    .await
}

/// An immutable audit namespace the audit worker must visit: it holds a live order, a recorded
/// checkpoint or a committed audit row (verifier/checkpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveredAuditNamespace {
    audit_tenant_id: Uuid,
}

/// Discover audit namespaces: up to `limit` **distinct** namespaces after the `after` keyset
/// cursor, so one namespace with many rows never hides the others.
///
/// D-100 item 2 enumerates **orders, not only surviving audit rows**, "so an empty/deleted
/// trail is a finding", and every order recorded by a previous checkpoint. The roster is
/// therefore the union of three sources, each probed on its own index: the immutable
/// `audit_tenant_id` of every live order (any counter: an empty tenant is checkpointed with
/// no members, a counter rolled back to zero above surviving rows is an overflow finding),
/// of every checkpoint header, and of every committed audit row. A namespace whose whole
/// committed trail was removed, or whose only order was removed after a checkpoint recorded
/// it, stays scheduled, so its checkpoint phase reports the missing heads or orders and its
/// verifier the incomplete chains instead of the namespace silently vanishing from the
/// schedule.
///
/// The union is a loose index scan of the three sources' indexes
/// (`discovery::audit_namespaces`).
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
            Ok(
                discovery::audit_namespaces(tx, &Reads(&discovery), after, limit)
                    .await?
                    .into_iter()
                    .map(|audit_tenant_id| DiscoveredAuditNamespace { audit_tenant_id })
                    .collect(),
            )
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
        let scope = if batch.scope_values.is_empty() {
            AccessScope::deny_all()
        } else {
            AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
                property,
                batch.scope_values.clone(),
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

    /// The scope writes to this target run under.
    pub(crate) fn access_scope(&self) -> &AccessScope {
        &self.scope
    }
    /// The discovered order, for an order target.
    pub(crate) fn order(&self) -> Option<&DiscoveredOrder> {
        match &self.target {
            Target::Order(row) => Some(row),
            _ => None,
        }
    }
    /// The discovered marker row, for an idempotency target.
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
    /// The audit namespace, for an audit namespace target.
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
