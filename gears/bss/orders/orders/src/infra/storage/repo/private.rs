//! Private persistence uses explicitly supplied service scopes; no allow-all construction.
use super::{
    AccessScope, DBRunner, EntityTrait, IntoActiveModel, QuerySelect, ScopeError, SecureEntityExt,
    SecureInsertExt, TransactionRunner, Uuid, order,
};

use crate::domain::idempotency::{RecordWindows, is_removable};
use crate::infra::maintenance::TargetScope;
use crate::infra::storage::entity;
use crate::infra::storage::sql;
use crate::infra::storage::status::{AttemptStatus, ControlStatus, RegistryStatus};
use sea_orm::QueryOrder;
use sea_orm::sea_query::{Expr, ExprTrait};

fn namespace_scope(
    target: &TargetScope,
    audit_tenant_id: Uuid,
) -> Result<&AccessScope, ScopeError> {
    if target.audit_namespace() != Some(audit_tenant_id) {
        return Err(ScopeError::Denied(
            "checkpoint row outside its discovered namespace",
        ));
    }
    Ok(target.access_scope())
}
// Audit, registry, diagnostic-run and checkpoint evidence must share the caller's transaction:
// a connection-level runner would let it commit apart from the mutation or sibling rows. These
// writers are single statements and take `&impl DBRunner` (TOOLKIT-DB-001); keeping them in the
// transaction is the job of their composite callers, which hold the sealed `TransactionRunner`
// (the locked aggregate, the idempotency gate, the checkpoint capture) and pass it down.
/// Raw row insert: production audit enters only through the sealed `repo::audit` writer; the
/// storage tests use it to exercise database CHECKs with deliberately malformed rows.
pub(in crate::infra::storage) async fn insert_transition_audit(
    runner: &impl DBRunner,
    scope: &AccessScope,
    row: entity::transition_audit::Model,
) -> Result<entity::transition_audit::Model, ScopeError> {
    let active = row.into_active_model();
    entity::transition_audit::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)?
        .exec_with_returning(runner)
        .await
}
/// Checkpoint writers accept only the discovered audit-namespace target (D-184).
pub async fn insert_audit_checkpoint(
    runner: &impl DBRunner,
    target: &TargetScope,
    row: entity::audit_checkpoint::Model,
) -> Result<entity::audit_checkpoint::Model, ScopeError> {
    let scope = namespace_scope(target, row.audit_tenant_id)?;
    let active = row.into_active_model();
    entity::audit_checkpoint::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)?
        .exec_with_returning(runner)
        .await
}
/// One page of checkpoint members in one multi-row statement (`secure_insert_many`, as the
/// policy-engine content repository writes a version's documents). Every row must belong to the
/// discovered namespace and is validated against its scope before anything is sent, so a
/// foreign row inserts nothing. The deferred header/member FK and cardinality triggers are
/// row-level and fire at commit, so batching changes no check.
pub async fn insert_audit_checkpoint_members(
    runner: &impl DBRunner,
    target: &TargetScope,
    rows: Vec<entity::audit_checkpoint_member::Model>,
) -> Result<(), ScopeError> {
    let Some(first) = rows.first() else {
        return Ok(());
    };
    let scope = namespace_scope(target, first.audit_tenant_id)?;
    if rows
        .iter()
        .any(|row| row.audit_tenant_id != first.audit_tenant_id)
    {
        return Err(ScopeError::Denied(
            "checkpoint row outside its discovered namespace",
        ));
    }
    let active = rows
        .into_iter()
        .map(IntoActiveModel::into_active_model)
        .collect();
    toolkit_db::secure::secure_insert_many::<entity::audit_checkpoint_member::Entity>(
        active, scope, runner,
    )
    .await
}
pub async fn insert_idempotency(
    runner: &impl DBRunner,
    scope: &AccessScope,
    row: entity::idempotency::Model,
) -> Result<entity::idempotency::Model, ScopeError> {
    let active = row.into_active_model();
    entity::idempotency::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)?
        .exec_with_returning(runner)
        .await
}
pub async fn insert_gate_outcome(
    runner: &impl DBRunner,
    scope: &AccessScope,
    row: entity::gate_outcome::Model,
) -> Result<entity::gate_outcome::Model, ScopeError> {
    let active = row.into_active_model();
    entity::gate_outcome::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)?
        .exec_with_returning(runner)
        .await
}
#[derive(Debug, sea_orm::FromQueryResult)]
struct ReadLogClock {
    t: time::OffsetDateTime,
}
/// Append one read access-log row (08 §3.7, S6-04). `accessed_at` is the database wall clock
/// read in the caller's transaction (`clock_timestamp()`), never the supplied value: like the
/// audit, claim and registry writers (OL-2), no application or caller clock reaches evidence.
/// The clock is projected through a select scoped to the new row's own `access_id` (the
/// aggregate yields one row although nothing matches yet); the insert itself is validated
/// against the subject-bound private read-log scope the caller passes.
pub async fn insert_read_access_log(
    runner: &impl DBRunner,
    scope: &AccessScope,
    mut row: entity::read_access_log::Model,
) -> Result<entity::read_access_log::Model, ScopeError> {
    let clock = entity::read_access_log::Entity::find()
        .secure()
        .scope_with(&AccessScope::for_resource(row.access_id))
        .project_all(runner, |q| {
            q.select_only()
                .column_as(sql::db_clock(), "t")
                .column_as(sql::row_anchor(), "n")
                .into_model::<ReadLogClock>()
        })
        .await?;
    row.accessed_at = clock
        .first()
        .map(|c| c.t)
        .ok_or(ScopeError::Invalid("database clock unavailable"))?;
    let active = row.into_active_model();
    entity::read_access_log::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)?
        .exec_with_returning(runner)
        .await
}
pub async fn insert_date_policy(
    runner: &impl DBRunner,
    scope: &AccessScope,
    row: entity::date_policy::Model,
) -> Result<entity::date_policy::Model, ScopeError> {
    let active = row.into_active_model();
    entity::date_policy::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)?
        .exec_with_returning(runner)
        .await
}
pub async fn insert_state_ttl_policy(
    runner: &impl DBRunner,
    scope: &AccessScope,
    row: entity::state_ttl_policy::Model,
) -> Result<entity::state_ttl_policy::Model, ScopeError> {
    let active = row.into_active_model();
    entity::state_ttl_policy::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)?
        .exec_with_returning(runner)
        .await
}
pub async fn insert_policy_election(
    runner: &impl DBRunner,
    scope: &AccessScope,
    row: entity::policy_election::Model,
) -> Result<entity::policy_election::Model, ScopeError> {
    let active = row.into_active_model();
    entity::policy_election::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)?
        .exec_with_returning(runner)
        .await
}

/// Maximum audit page (D-101 / 08 §2.2 list bound).
pub const MAX_AUDIT_PAGE: u64 = 200;

/// Resolved audit disclosure always follows current parent authorization, including old payer
/// snapshots. An immutable audit namespace is never a business-read permission.
///
/// One keyset page ordered by `(created_at, audit_id)`; `after` is the exclusive cursor of the
/// previous page. A caller must page explicitly rather than receive a silently truncated trail.
pub async fn resolved_audit_page(
    runner: &impl DBRunner,
    order_scope: &AccessScope,
    order_id: Uuid,
    after: Option<(time::OffsetDateTime, Uuid)>,
    limit: u64,
) -> Result<Vec<entity::transition_audit::Model>, ScopeError> {
    use entity::transition_audit::Column;
    use sea_orm::{ColumnTrait, Condition, QueryFilter, QueryOrder, QuerySelect};
    if limit == 0 || limit > MAX_AUDIT_PAGE {
        return Err(ScopeError::Invalid("audit page size must be 1..=200"));
    }
    let mut query = entity::transition_audit::Entity::find()
        .inner_join(order::Entity)
        .filter(Column::OrderId.eq(order_id));
    if let Some((created_at, audit_id)) = after {
        query = query.filter(
            Condition::any().add(Column::CreatedAt.gt(created_at)).add(
                Condition::all()
                    .add(Column::CreatedAt.eq(created_at))
                    .add(Column::AuditId.gt(audit_id)),
            ),
        );
    }
    query
        .order_by_asc(Column::CreatedAt)
        .order_by_asc(Column::AuditId)
        .limit(limit)
        .secure()
        .scope_with(&AccessScope::for_resources(vec![order_id]))
        .and_scope_for::<order::Entity>(order_scope)
        .all(runner)
        .await
}

/// The primary key of an idempotency registry row, as its three stored columns. Named fields,
/// so the operation, principal scope and key text cannot be passed in each other's place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryKeyColumns<'a> {
    pub operation: &'a str,
    pub principal_scope: &'a str,
    pub idempotency_key: &'a str,
}

impl<'a> RegistryKeyColumns<'a> {
    /// The key of a stored registry row.
    #[must_use]
    pub fn of_row(row: &'a entity::idempotency::Model) -> Self {
        Self {
            operation: &row.operation,
            principal_scope: &row.principal_scope,
            idempotency_key: &row.idempotency_key,
        }
    }

    fn primary_key(self) -> (String, String, String) {
        (
            self.operation.to_owned(),
            self.principal_scope.to_owned(),
            self.idempotency_key.to_owned(),
        )
    }

    /// `query` restricted to this key.
    pub fn filter(
        self,
        query: sea_orm::Select<entity::idempotency::Entity>,
    ) -> sea_orm::Select<entity::idempotency::Entity> {
        use sea_orm::{ColumnTrait, QueryFilter};
        query
            .filter(entity::idempotency::Column::Operation.eq(self.operation))
            .filter(entity::idempotency::Column::PrincipalScope.eq(self.principal_scope))
            .filter(entity::idempotency::Column::IdempotencyKey.eq(self.idempotency_key))
    }
}

pub async fn find_idempotency(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: RegistryKeyColumns<'_>,
) -> Result<Option<entity::idempotency::Model>, ScopeError> {
    entity::idempotency::Entity::find_by_id(key.primary_key())
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}
/// `FOR UPDATE` on the registry row, so it keeps the sealed `TransactionRunner`: on a connection
/// the lock would end with its own autocommit statement and serialize nothing.
pub async fn lock_idempotency(
    tx: &impl TransactionRunner,
    scope: &AccessScope,
    key: RegistryKeyColumns<'_>,
) -> Result<Option<entity::idempotency::Model>, ScopeError> {
    entity::idempotency::Entity::find_by_id(key.primary_key())
        .lock_exclusive()
        .secure()
        .scope_with(scope)
        .one(tx)
        .await
}
/// A conflict grants no ownership; the caller must lock/re-read the winning record.
pub async fn offer_idempotency(
    tx: &impl DBRunner,
    scope: &AccessScope,
    row: entity::idempotency::Model,
) -> Result<bool, ScopeError> {
    let active = row.into_active_model();
    let result = entity::idempotency::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)?
        .on_conflict_raw(
            sea_orm::sea_query::OnConflict::columns([
                entity::idempotency::Column::Operation,
                entity::idempotency::Column::PrincipalScope,
                entity::idempotency::Column::IdempotencyKey,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(tx)
        .await;
    match result {
        Ok(_) => Ok(true),
        Err(ScopeError::Db(sea_orm::DbErr::RecordNotInserted)) => Ok(false),
        Err(e) => Err(e),
    }
}
/// Registry cleanup accepts only a discovered marker target (D-184). The statement itself
/// rechecks the window against fresh database time (an unexpired or still-leased generation
/// matches nothing), and the DB guard independently refuses live windows and unresolved
/// executions.
pub async fn delete_expired_idempotency(
    tx: &impl DBRunner,
    target: &TargetScope,
) -> Result<bool, ScopeError> {
    use sea_orm::{ColumnTrait, Condition, QueryFilter};
    use toolkit_db::secure::SecureDeleteExt;
    let row = target
        .idempotency()
        .ok_or(ScopeError::Invalid("not an idempotency maintenance target"))?;
    let scope = target.access_scope();
    let result = entity::idempotency::Entity::delete_many()
        .filter(entity::idempotency::Column::Operation.eq(&row.operation))
        .filter(entity::idempotency::Column::PrincipalScope.eq(&row.principal_scope))
        .filter(entity::idempotency::Column::IdempotencyKey.eq(&row.idempotency_key))
        .filter(entity::idempotency::Column::ExecutionId.eq(row.execution_id))
        .filter(Expr::col(entity::idempotency::Column::ExpiresAt).lt(sql::db_clock()))
        .filter(
            Condition::any()
                .add(entity::idempotency::Column::Status.ne(RegistryStatus::InFlight.token()))
                .add(entity::idempotency::Column::LeaseExpiresAt.is_null())
                .add(Expr::col(entity::idempotency::Column::LeaseExpiresAt).lt(sql::db_clock())),
        )
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await?;
    Ok(result.rows_affected == 1)
}

/// Why a discovered expired marker was not deleted by [`sweep_expired_marker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepOutcome {
    /// The exact discovered generation was deleted.
    Deleted,
    /// The key now carries another generation (replaced or reclaimed since discovery).
    Replaced,
    /// Rechecked under the row lock, the window or lease is live again: never deleted.
    Live,
    /// The marker links an unresolved D-188/D-198 execution: preserved for recovery.
    Unresolved,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct ClockRow {
    observed_at: time::OffsetDateTime,
}

/// Fresh database wall-clock time read through the target's registry scope (the aggregate
/// form returns one row even when nothing matches; the constant false filter keeps PostgreSQL
/// from reading the rows a count nobody reads would cover, as in `discover_clock`).
async fn registry_clock(
    tx: &impl DBRunner,
    scope: &AccessScope,
) -> Result<time::OffsetDateTime, ScopeError> {
    use sea_orm::QueryFilter;
    let rows = entity::idempotency::Entity::find()
        .secure()
        .scope_with(scope)
        .project_all(tx, |q| {
            q.select_only()
                .column_as(sql::db_clock(), "observed_at")
                .column_as(sql::row_anchor(), "n")
                .filter(sql::no_rows())
                .into_model::<ClockRow>()
        })
        .await?;
    rows.first()
        .map(|r| r.observed_at)
        .ok_or(ScopeError::Invalid("database clock unavailable"))
}

/// Idempotency-window cleanup of one discovered marker (01 §3.1 item 6; DESIGN §3.8 roster
/// "recheck expiry and settlement under row lock; never delete a live/reclaimed in-flight
/// record"). The maintenance role holds SELECT and DELETE only (no UPDATE, so no explicit
/// `FOR UPDATE`): the row lock is the conditional DELETE's own, whose predicates (exact
/// discovered generation, retention window and lease against fresh database time) PostgreSQL
/// re-evaluates on the current row version after any concurrent writer commits, and the
/// registry guard trigger rechecks live windows and unresolved executions on that same row.
/// Before deleting, the current row is read to classify a replaced generation, a live window
/// or an unresolved linked execution, each preserved.
///
/// # Errors
/// A non-marker target, scope or store failure.
///
/// Several statements (current row, clock, linked executions, conditional delete, re-read)
/// classify one marker against one transaction's view, so it keeps the sealed
/// `TransactionRunner` rather than the single-statement `DBRunner` bound.
pub async fn sweep_expired_marker(
    tx: &impl TransactionRunner,
    target: &TargetScope,
) -> Result<SweepOutcome, ScopeError> {
    use sea_orm::{ColumnTrait, QueryFilter};
    let row = target
        .idempotency()
        .ok_or(ScopeError::Invalid("not an idempotency maintenance target"))?;
    let scope = target.access_scope();
    let current = |tx| find_idempotency(tx, scope, RegistryKeyColumns::of_row(row));
    let Some(observed) = current(tx).await? else {
        return Ok(SweepOutcome::Replaced);
    };
    if observed.execution_id != row.execution_id {
        return Ok(SweepOutcome::Replaced);
    }
    let now = registry_clock(tx, scope).await?;
    let windows = RecordWindows {
        expires_at: observed.expires_at,
        lease_expires_at: observed
            .lease_expires_at
            .filter(|_| observed.status == RegistryStatus::InFlight.token()),
    };
    if !windows.elapsed(now) {
        return Ok(SweepOutcome::Live);
    }
    // The linked execution is read under the marker's own persisted order ID (D-184: a
    // discovered identifier, never caller input). Unresolved work survives retention.
    if let (Some(attempt_id), Some(order_id)) = (observed.attempt_id, observed.order_id) {
        let unresolved = entity::commercial_attempt::Entity::find_by_id(attempt_id)
            .filter(
                entity::commercial_attempt::Column::Status
                    .is_in(AttemptStatus::unresolved_tokens()),
            )
            .secure()
            .scope_with(&AccessScope::for_resource(order_id))
            .one(tx)
            .await?
            .is_some();
        if !is_removable(windows, unresolved, now) {
            return Ok(SweepOutcome::Unresolved);
        }
    }
    if let (Some(control_id), Some(order_id)) = (observed.fulfillment_control_id, observed.order_id)
    {
        let unresolved = entity::fulfillment_control::Entity::find_by_id(control_id)
            .filter(
                entity::fulfillment_control::Column::Status
                    .is_in(ControlStatus::unresolved_tokens()),
            )
            .secure()
            .scope_with(&AccessScope::for_resource(order_id))
            .one(tx)
            .await?
            .is_some();
        if !is_removable(windows, unresolved, now) {
            return Ok(SweepOutcome::Unresolved);
        }
    }
    if delete_expired_idempotency(tx, target).await? {
        return Ok(SweepOutcome::Deleted);
    }
    // The conditional delete matched nothing on the current row version: a concurrent writer
    // replaced or reclaimed the generation in the meantime.
    Ok(match current(tx).await? {
        Some(latest) if latest.execution_id == row.execution_id => SweepOutcome::Live,
        _ => SweepOutcome::Replaced,
    })
}

pub async fn find_date_policy(
    runner: &impl DBRunner,
    scope: &AccessScope,
    policy_id: Uuid,
) -> Result<Option<entity::date_policy::Model>, ScopeError> {
    entity::date_policy::Entity::find_by_id(policy_id)
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

/// The date-policy rows `scope` admits (S2-10), optionally locked `FOR UPDATE` for promotion.
pub async fn date_policy_rows(
    runner: &impl DBRunner,
    scope: &AccessScope,
    lock: bool,
) -> Result<Vec<entity::date_policy::Model>, ScopeError> {
    let query =
        entity::date_policy::Entity::find().order_by_asc(entity::date_policy::Column::PolicyId);
    let query = if lock { query.lock_exclusive() } else { query };
    query.secure().scope_with(scope).all(runner).await
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct ObservedPolicy {
    policy_id: Uuid,
    resource_tenant_id: Option<Uuid>,
    service_activation_required: bool,
    acceptance_due_required: bool,
    revision: i64,
    updated_at: time::OffsetDateTime,
    observed_at: time::OffsetDateTime,
}
/// The date-policy rows `scope` admits together with the database clock (`clock_timestamp()`)
/// observed by the same statement: the snapshot and the proposed UTC date share the engine's
/// time source. `None` when no row is visible (the clock is then not needed: the date guard
/// fails without a default).
pub async fn date_policy_rows_at(
    runner: &impl DBRunner,
    scope: &AccessScope,
) -> Result<Option<(Vec<entity::date_policy::Model>, time::OffsetDateTime)>, ScopeError> {
    use entity::date_policy::Column as C;
    let rows = entity::date_policy::Entity::find()
        .order_by_asc(C::PolicyId)
        .secure()
        .scope_with(scope)
        .project_all(runner, |q| {
            q.select_only()
                .columns([
                    C::PolicyId,
                    C::ResourceTenantId,
                    C::ServiceActivationRequired,
                    C::AcceptanceDueRequired,
                    C::Revision,
                    C::UpdatedAt,
                ])
                .column_as(sql::db_clock(), "observed_at")
                .into_model::<ObservedPolicy>()
        })
        .await?;
    let Some(observed_at) = rows.iter().map(|r| r.observed_at).max() else {
        return Ok(None);
    };
    Ok(Some((
        rows.into_iter()
            .map(|r| entity::date_policy::Model {
                policy_id: r.policy_id,
                resource_tenant_id: r.resource_tenant_id,
                service_activation_required: r.service_activation_required,
                acceptance_due_required: r.acceptance_due_required,
                revision: r.revision,
                updated_at: r.updated_at,
            })
            .collect(),
        observed_at,
    )))
}
pub async fn find_state_ttl_policy(
    runner: &impl DBRunner,
    scope: &AccessScope,
    policy_id: Uuid,
) -> Result<Option<entity::state_ttl_policy::Model>, ScopeError> {
    entity::state_ttl_policy::Entity::find_by_id(policy_id)
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

pub async fn find_policy_election(
    runner: &impl DBRunner,
    scope: &AccessScope,
    policy_id: Uuid,
) -> Result<Option<entity::policy_election::Model>, ScopeError> {
    entity::policy_election::Entity::find_by_id(policy_id)
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

pub async fn find_transition_audit(
    runner: &impl DBRunner,
    scope: &AccessScope,
    audit_id: Uuid,
) -> Result<Option<entity::transition_audit::Model>, ScopeError> {
    entity::transition_audit::Entity::find_by_id(audit_id)
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

pub async fn find_read_access_log(
    runner: &impl DBRunner,
    scope: &AccessScope,
    access_id: Uuid,
) -> Result<Option<entity::read_access_log::Model>, ScopeError> {
    entity::read_access_log::Entity::find_by_id(access_id)
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}
