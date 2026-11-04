//! Maintenance discovery queries (08 §3.5, D-184): the statement bodies the maintenance
//! discovery entry points run.
//!
//! The capability stays in `infra::maintenance::scope`: its private `DiscoveryScope` is the only
//! holder of `AccessScope::allow_all`, its entry points check the task grant and the batch bound
//! and open the `TxConfig::read_only` transaction, and only it can build the owned discovered
//! rows and the write-side `TargetScope`. It hands these functions a [`DiscoveryReads`], which
//! scopes a **select** and does nothing else, so no query here can write, see a raw scope or
//! construct a discovered row; each returns plain stored values.
//!
//! This file depends only on `super::{entity, sql, status}` and toolkit crates, so the D-184
//! compile-fail fixtures can include it unchanged beside the capability module.
use super::entity::{
    audit_checkpoint, commercial_attempt, fulfillment_control, gate_outcome, idempotency, order,
    read_access_log, transition_audit,
};
use super::sql;
use super::status::{AttemptStatus, ControlStatus};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Select};
use time::OffsetDateTime;
use toolkit_db::DbTx;
use toolkit_db::secure::{ScopableEntity, Scoped, SecureSelect};
use uuid::Uuid;

/// The discovery capability as these queries see it: a select scoped for read-only discovery.
pub trait DiscoveryReads: Sync {
    /// `query` under the discovery scope.
    fn scoped<E>(&self, query: Select<E>) -> SecureSelect<E, Scoped>
    where
        E: ScopableEntity + EntityTrait,
        E::Model: Send + Sync;
}

/// Orders in `state` that entered it before `entered_before`, oldest first, at most `limit`.
///
/// # Errors
/// Store failure.
pub async fn due_orders(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    state: String,
    entered_before: OffsetDateTime,
    limit: u64,
) -> anyhow::Result<Vec<order::Model>> {
    Ok(reads
        .scoped(
            order::Entity::find()
                .filter(order::Column::State.eq(state))
                .filter(order::Column::StateEnteredAt.lt(entered_before))
                .order_by_asc(order::Column::StateEnteredAt)
                .order_by_asc(order::Column::OrderId)
                .limit(limit),
        )
        .all(tx)
        .await?)
}

/// Registry markers whose window expired before `now`, strictly after the keyset position
/// `after` in `(expires_at, execution_id)` order, at most `limit`.
///
/// # Errors
/// Store failure.
pub async fn expired_markers(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    now: OffsetDateTime,
    after: Option<(OffsetDateTime, Uuid)>,
    limit: u64,
) -> anyhow::Result<Vec<idempotency::Model>> {
    let mut query = idempotency::Entity::find().filter(idempotency::Column::ExpiresAt.lt(now));
    if let Some((expires_at, execution_id)) = after {
        query = query.filter(
            Condition::any()
                .add(idempotency::Column::ExpiresAt.gt(expires_at))
                .add(
                    Condition::all()
                        .add(idempotency::Column::ExpiresAt.eq(expires_at))
                        .add(idempotency::Column::ExecutionId.gt(execution_id)),
                ),
        );
    }
    Ok(reads
        .scoped(
            query
                .order_by_asc(idempotency::Column::ExpiresAt)
                .order_by_asc(idempotency::Column::ExecutionId)
                .limit(limit),
        )
        .all(tx)
        .await?)
}

/// Expired refused audit rows: `(audit_id, subject_tenant_id)`, oldest first.
///
/// # Errors
/// Store failure.
pub async fn refused_audit_candidates(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    cutoff: OffsetDateTime,
    limit: u64,
) -> anyhow::Result<Vec<(Uuid, Uuid)>> {
    Ok(reads
        .scoped(
            refused_audit_before(cutoff)
                .order_by_asc(transition_audit::Column::CreatedAt)
                .order_by_asc(transition_audit::Column::AuditId)
                .limit(limit),
        )
        .all(tx)
        .await?
        .into_iter()
        .map(|r| (r.audit_id, r.subject_tenant_id))
        .collect())
}

/// Expired preview gate outcomes (no order attached): `(outcome_id, subject_tenant_id)`, oldest
/// first.
///
/// # Errors
/// Store failure.
pub async fn preview_diagnostic_candidates(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    cutoff: OffsetDateTime,
    limit: u64,
) -> anyhow::Result<Vec<(Uuid, Uuid)>> {
    Ok(reads
        .scoped(
            preview_diagnostics_before(cutoff)
                .order_by_asc(gate_outcome::Column::EvaluatedAt)
                .order_by_asc(gate_outcome::Column::OutcomeId)
                .limit(limit),
        )
        .all(tx)
        .await?
        .into_iter()
        .map(|r| (r.outcome_id, r.subject_tenant_id))
        .collect())
}

/// Expired read access-log rows: their `access_id`s, oldest first.
///
/// # Errors
/// Store failure.
pub async fn access_log_candidates(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    cutoff: OffsetDateTime,
    limit: u64,
) -> anyhow::Result<Vec<Uuid>> {
    Ok(reads
        .scoped(
            access_log_before(cutoff)
                .order_by_asc(read_access_log::Column::AccessedAt)
                .order_by_asc(read_access_log::Column::AccessId)
                .limit(limit),
        )
        .all(tx)
        .await?
        .into_iter()
        .map(|r| r.access_id)
        .collect())
}

fn refused_audit_before(cutoff: OffsetDateTime) -> Select<transition_audit::Entity> {
    transition_audit::Entity::find()
        .filter(transition_audit::Column::Outcome.eq("refused"))
        .filter(transition_audit::Column::CreatedAt.lt(cutoff))
}
fn preview_diagnostics_before(cutoff: OffsetDateTime) -> Select<gate_outcome::Entity> {
    gate_outcome::Entity::find()
        .filter(gate_outcome::Column::OrderId.is_null())
        .filter(gate_outcome::Column::EvaluatedAt.lt(cutoff))
}
fn access_log_before(cutoff: OffsetDateTime) -> Select<read_access_log::Entity> {
    read_access_log::Entity::find().filter(read_access_log::Column::AccessedAt.lt(cutoff))
}

/// Count of expired refused audit rows and the oldest `created_at`.
///
/// # Errors
/// Store failure.
pub async fn refused_audit_backlog(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    cutoff: OffsetDateTime,
) -> anyhow::Result<(u64, Option<OffsetDateTime>)> {
    let query = refused_audit_before(cutoff);
    let rows = reads.scoped(query.clone()).count(tx).await?;
    let oldest = reads
        .scoped(
            query
                .order_by_asc(transition_audit::Column::CreatedAt)
                .limit(1),
        )
        .one(tx)
        .await?
        .map(|r| r.created_at);
    Ok((rows, oldest))
}

/// Count of expired preview gate outcomes and the oldest `evaluated_at`.
///
/// # Errors
/// Store failure.
pub async fn preview_diagnostics_backlog(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    cutoff: OffsetDateTime,
) -> anyhow::Result<(u64, Option<OffsetDateTime>)> {
    let query = preview_diagnostics_before(cutoff);
    let rows = reads.scoped(query.clone()).count(tx).await?;
    let oldest = reads
        .scoped(
            query
                .order_by_asc(gate_outcome::Column::EvaluatedAt)
                .limit(1),
        )
        .one(tx)
        .await?
        .map(|r| r.evaluated_at);
    Ok((rows, oldest))
}

/// Count of expired read access-log rows and the oldest `accessed_at`.
///
/// # Errors
/// Store failure.
pub async fn access_log_backlog(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    cutoff: OffsetDateTime,
) -> anyhow::Result<(u64, Option<OffsetDateTime>)> {
    let query = access_log_before(cutoff);
    let rows = reads.scoped(query.clone()).count(tx).await?;
    let oldest = reads
        .scoped(
            query
                .order_by_asc(read_access_log::Column::AccessedAt)
                .limit(1),
        )
        .one(tx)
        .await?
        .map(|r| r.accessed_at);
    Ok((rows, oldest))
}

/// Count of expired registry markers and the oldest `expires_at`.
///
/// # Errors
/// Store failure.
pub async fn marker_backlog(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    now: OffsetDateTime,
) -> anyhow::Result<(u64, Option<OffsetDateTime>)> {
    let query = idempotency::Entity::find().filter(idempotency::Column::ExpiresAt.lt(now));
    let rows = reads.scoped(query.clone()).count(tx).await?;
    let oldest = reads
        .scoped(query.order_by_asc(idempotency::Column::ExpiresAt).limit(1))
        .one(tx)
        .await?
        .map(|r| r.expires_at);
    Ok((rows, oldest))
}

/// Lapsed unresolved executions: each table's bounded page after the keyset position, plus the
/// whole-set count and oldest lease.
#[derive(Debug)]
pub struct LapsedRows {
    /// Up to `limit` attempts after the cursor, in `(lease_until, attempt_id)` order.
    pub attempts: Vec<commercial_attempt::Model>,
    /// Up to `limit` controls after the cursor, in `(lease_until, control_id)` order.
    pub controls: Vec<fulfillment_control::Model>,
    /// Lapsed unresolved executions in both tables.
    pub total: u64,
    /// The oldest lapsed lease in both tables.
    pub oldest_lease: Option<OffsetDateTime>,
}

/// Unresolved attempts and controls whose lease lapsed before `now`, strictly after the keyset
/// position `after` (`(lease_until, id)`), plus the backlog figures of the whole set.
///
/// # Errors
/// Store failure.
pub async fn lapsed_executions(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    now: OffsetDateTime,
    after: Option<(OffsetDateTime, Uuid)>,
    limit: u64,
) -> anyhow::Result<LapsedRows> {
    let mut attempts = commercial_attempt::Entity::find()
        .filter(commercial_attempt::Column::Status.is_in(AttemptStatus::unresolved_tokens()))
        .filter(commercial_attempt::Column::LeaseUntil.lt(now));
    let mut controls = fulfillment_control::Entity::find()
        .filter(fulfillment_control::Column::Status.is_in(ControlStatus::unresolved_tokens()))
        .filter(fulfillment_control::Column::LeaseUntil.lt(now));
    let total = reads.scoped(attempts.clone()).count(tx).await?
        + reads.scoped(controls.clone()).count(tx).await?;
    let oldest_attempt = reads
        .scoped(
            attempts
                .clone()
                .order_by_asc(commercial_attempt::Column::LeaseUntil)
                .limit(1),
        )
        .one(tx)
        .await?
        .and_then(|a| a.lease_until);
    let oldest_control = reads
        .scoped(
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
    if let Some((lease_until, id)) = after {
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
    let attempts = reads
        .scoped(
            attempts
                .order_by_asc(commercial_attempt::Column::LeaseUntil)
                .order_by_asc(commercial_attempt::Column::AttemptId)
                .limit(limit),
        )
        .all(tx)
        .await?;
    let controls = reads
        .scoped(
            controls
                .order_by_asc(fulfillment_control::Column::LeaseUntil)
                .order_by_asc(fulfillment_control::Column::ControlId)
                .limit(limit),
        )
        .all(tx)
        .await?;
    Ok(LapsedRows {
        attempts,
        controls,
        total,
        oldest_lease,
    })
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct ClockRow {
    observed_at: OffsetDateTime,
}

/// Which table a discovery clock probe reads through: the retention role reads evidence stores
/// only, every other maintenance class reads the aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockTable {
    /// `transition_audit`, readable by the retention role.
    Evidence,
    /// `order`, readable by every other maintenance class.
    Aggregate,
}

/// Fresh database wall-clock time (`clock_timestamp()`) through `table`. The aggregate form
/// returns one row even when the probed table is empty, and the constant false filter keeps
/// PostgreSQL from scanning the table for a count nobody reads.
///
/// # Errors
/// Store failure, or no row.
pub async fn clock(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    table: ClockTable,
) -> anyhow::Result<OffsetDateTime> {
    let rows = match table {
        ClockTable::Evidence => {
            reads
                .scoped(transition_audit::Entity::find())
                .project_all(tx, |q| {
                    q.select_only()
                        .column_as(sql::db_clock(), "observed_at")
                        .column_as(sql::row_anchor(), "n")
                        .filter(sql::no_rows())
                        .into_model::<ClockRow>()
                })
                .await?
        }
        ClockTable::Aggregate => {
            reads
                .scoped(order::Entity::find())
                .project_all(tx, |q| {
                    q.select_only()
                        .column_as(sql::db_clock(), "observed_at")
                        .column_as(sql::row_anchor(), "n")
                        .filter(sql::no_rows())
                        .into_model::<ClockRow>()
                })
                .await?
        }
    };
    rows.first()
        .map(|r| r.observed_at)
        .ok_or_else(|| anyhow::anyhow!("database clock unavailable"))
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct NamespaceRow {
    audit_tenant_id: Uuid,
}

/// The three namespace sources of the audit roster (D-100 item 2).
#[derive(Debug, Clone, Copy)]
enum NamespaceSource {
    /// Every live order's immutable `audit_tenant_id`.
    Orders,
    /// Every checkpoint header.
    Checkpoints,
    /// Every committed audit row.
    Committed,
}

/// The smallest namespace of one source strictly after `after`: one index probe
/// (`ORDER BY audit_tenant_id LIMIT 1` on the source's index led by `audit_tenant_id`), so a
/// namespace's rows are never walked to reach the next one.
async fn next_namespace(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    source: NamespaceSource,
    after: Option<Uuid>,
) -> anyhow::Result<Option<Uuid>> {
    let rows = match source {
        NamespaceSource::Orders => {
            let mut query = order::Entity::find();
            if let Some(after) = after {
                query = query.filter(order::Column::AuditTenantId.gt(after));
            }
            reads
                .scoped(query)
                .project_all(tx, |q| {
                    q.select_only()
                        .column(order::Column::AuditTenantId)
                        .order_by_asc(order::Column::AuditTenantId)
                        .limit(1)
                        .into_model::<NamespaceRow>()
                })
                .await?
        }
        NamespaceSource::Checkpoints => {
            let mut query = audit_checkpoint::Entity::find();
            if let Some(after) = after {
                query = query.filter(audit_checkpoint::Column::AuditTenantId.gt(after));
            }
            reads
                .scoped(query)
                .project_all(tx, |q| {
                    q.select_only()
                        .column(audit_checkpoint::Column::AuditTenantId)
                        .order_by_asc(audit_checkpoint::Column::AuditTenantId)
                        .limit(1)
                        .into_model::<NamespaceRow>()
                })
                .await?
        }
        NamespaceSource::Committed => {
            let mut query = transition_audit::Entity::find()
                .filter(transition_audit::Column::Outcome.eq("committed"))
                .filter(transition_audit::Column::AuditTenantId.is_not_null());
            if let Some(after) = after {
                query = query.filter(transition_audit::Column::AuditTenantId.gt(after));
            }
            reads
                .scoped(query)
                .project_all(tx, |q| {
                    q.select_only()
                        .column(transition_audit::Column::AuditTenantId)
                        .order_by_asc(transition_audit::Column::AuditTenantId)
                        .limit(1)
                        .into_model::<NamespaceRow>()
                })
                .await?
        }
    };
    Ok(rows.first().map(|r| r.audit_tenant_id))
}

/// Up to `limit` distinct audit namespaces after `after`, ascending, from the union of the live
/// orders, the checkpoint headers and the committed audit rows.
///
/// The union is a loose index scan: each source holds its next namespace after the cursor, the
/// smallest of them is emitted, and only the sources that held it are probed again. `n`
/// namespaces cost at most `3 + 3n` single-row index probes, whatever the number of rows behind
/// each namespace, instead of a `DISTINCT` that walks every committed audit row to prove there
/// are no more (the existing `audit_tenant_id`-led indexes serve every probe; no migration).
///
/// # Errors
/// Store failure.
pub async fn audit_namespaces(
    tx: &DbTx<'_>,
    reads: &impl DiscoveryReads,
    after: Option<Uuid>,
    limit: u64,
) -> anyhow::Result<Vec<Uuid>> {
    const SOURCES: [NamespaceSource; 3] = [
        NamespaceSource::Orders,
        NamespaceSource::Checkpoints,
        NamespaceSource::Committed,
    ];
    let mut heads = [None; 3];
    for (head, source) in heads.iter_mut().zip(SOURCES) {
        *head = next_namespace(tx, reads, source, after).await?;
    }
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut found = Vec::new();
    while found.len() < limit {
        let Some(next) = heads.iter().flatten().min().copied() else {
            break;
        };
        found.push(next);
        for (head, source) in heads.iter_mut().zip(SOURCES) {
            if *head == Some(next) {
                *head = next_namespace(tx, reads, source, Some(next)).await?;
            }
        }
    }
    Ok(found)
}
