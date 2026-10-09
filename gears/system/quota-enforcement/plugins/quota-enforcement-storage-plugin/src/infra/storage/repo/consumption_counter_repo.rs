//! `qe_quota_consumption_counters`: the per-period counter rows of consumption
//! Quotas.
//!
//! Lock order is the Quota row, then its counter rows (ADR-0002); every read
//! here is taken after the Quota is locked. Counter writes are compare-and-set
//! on `record_version`, so a row that moved between the read and the write
//! reports it instead of silently overwriting.

use quota_enforcement_sdk::PeriodWindow;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue, ColumnTrait, DbErr, EntityTrait, Order, QueryFilter, QueryOrder};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt, secure_insert,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::RowWait;
use crate::infra::storage::entity::quota;
use crate::infra::storage::entity::quota_consumption_counter::{self, Column, Entity};

/// The Quota's most recent period row, locked for update. `None` when the
/// Quota has never been evaluated.
///
/// The latest row is the only candidate for the current period: rows are
/// materialized in calendar order, so an earlier one cannot contain a later
/// instant.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find_latest_for_update(
    runner: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    wait: RowWait,
) -> Result<Option<quota_consumption_counter::Model>, ScopeError> {
    wait.apply(
        Entity::find()
            .filter(Column::QuotaId.eq(quota_id))
            .order_by(Column::PeriodStart, Order::Desc),
    )
    .secure()
    .scope_with(scope)
    .one(runner)
    .await
}

/// The Quota's most recent period row, unlocked, for a snapshot read.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find_latest(
    runner: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
) -> Result<Option<quota_consumption_counter::Model>, ScopeError> {
    Entity::find()
        .filter(Column::QuotaId.eq(quota_id))
        .order_by(Column::PeriodStart, Order::Desc)
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

/// One period row by id, locked for update. Rollback reaches its acquisition
/// period this way, which is not the current one (I5).
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find_by_period_id_for_update(
    runner: &impl DBRunner,
    scope: &AccessScope,
    period_id: Uuid,
    wait: RowWait,
) -> Result<Option<quota_consumption_counter::Model>, ScopeError> {
    wait.apply(Entity::find().filter(Column::PeriodId.eq(period_id)))
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

/// Every unsettled row of `quota_id` that ended at or before `now`, locked.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find_elapsed_unsettled_for_update(
    runner: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    now: OffsetDateTime,
    wait: RowWait,
) -> Result<Vec<quota_consumption_counter::Model>, ScopeError> {
    wait.apply(
        Entity::find()
            .filter(Column::QuotaId.eq(quota_id))
            .filter(Column::IsSettled.eq(false))
            .filter(Column::PeriodEnd.lte(now))
            .order_by(Column::PeriodStart, Order::Asc),
    )
    .secure()
    .scope_with(scope)
    .all(runner)
    .await
}

/// Parameters of a materialized period row.
pub struct NewPeriod {
    /// The Quota the period belongs to.
    pub quota_id: Uuid,
    /// The Quota's tenant.
    pub tenant_id: Uuid,
    /// Period start, inclusive.
    pub start: OffsetDateTime,
    /// Period end, exclusive; the sentinel for a one-time Quota.
    pub end: OffsetDateTime,
    /// Materialization time.
    pub now: OffsetDateTime,
}

/// Materialize a period row with a zero counter and no threshold marker (I13).
///
/// The unique key on `(quota_id, period_start)` arbitrates two transactions
/// opening the same period: the loser sees a unique violation and reads the
/// winner's row.
///
/// # Errors
///
/// The scope or database error of the insert, a unique violation included.
pub async fn insert_period(
    runner: &impl DBRunner,
    scope: &AccessScope,
    period_id: Uuid,
    period: &NewPeriod,
) -> Result<quota_consumption_counter::Model, ScopeError> {
    secure_insert::<Entity>(
        quota_consumption_counter::ActiveModel {
            period_id: ActiveValue::Set(period_id),
            quota_id: ActiveValue::Set(period.quota_id),
            tenant_id: ActiveValue::Set(period.tenant_id),
            period_start: ActiveValue::Set(period.start),
            period_end: ActiveValue::Set(period.end),
            consumed: ActiveValue::Set(0),
            highest_crossed_threshold_pct: ActiveValue::Set(None),
            is_settled: ActiveValue::Set(false),
            record_version: ActiveValue::Set(1),
            created_at: ActiveValue::Set(period.now),
            updated_at: ActiveValue::Set(period.now),
        },
        scope,
        runner,
    )
    .await
}

/// Materialize the current window's period row of a Quota a snapshot read has
/// just read (the I3 exception), keeping a row another transaction created
/// first. Returns whether this call inserted it.
///
/// # Precondition: `quota` was read under `scope`, in this transaction
///
/// The row's tenant and Quota ids come only from `quota`, never from request
/// input: the caller passes the Quota row its own transaction read under the
/// caller's PDP scope, and that read is the authority for the insert. The
/// scope is carried unchanged but not re-checked in memory, because
/// `secure_insert` cannot evaluate a tenant-subtree filter against a row and
/// would refuse the insert under such a grant. This is the plugin's only
/// unchecked insert, and only the snapshot read path calls it.
///
/// No explicit row lock is taken; a conflicting insert of the same period may
/// still wait for the other transaction to end. The row starts at zero with
/// no threshold marker (I13), and nothing is settled or emitted.
///
/// # Errors
///
/// The scope or database error of the insert.
pub(in crate::infra::storage) async fn insert_current_period_of(
    runner: &impl DBRunner,
    scope: &AccessScope,
    quota: &quota::Model,
    window: &PeriodWindow,
    now: OffsetDateTime,
) -> Result<bool, ScopeError> {
    let row = quota_consumption_counter::ActiveModel {
        period_id: ActiveValue::Set(Uuid::now_v7()),
        quota_id: ActiveValue::Set(quota.id),
        tenant_id: ActiveValue::Set(quota.tenant_id),
        period_start: ActiveValue::Set(window.start),
        period_end: ActiveValue::Set(window.end),
        consumed: ActiveValue::Set(0),
        highest_crossed_threshold_pct: ActiveValue::Set(None),
        is_settled: ActiveValue::Set(false),
        record_version: ActiveValue::Set(1),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
    };
    let keep_existing = OnConflict::columns([Column::QuotaId, Column::PeriodStart])
        .do_nothing()
        .to_owned();
    let inserted = Entity::insert(row)
        .secure()
        .scope_unchecked(scope)?
        // Nothing is updated on conflict, so the tenant cannot change.
        .on_conflict_raw(keep_existing)
        .exec(runner)
        .await;
    match inserted {
        Ok(_) => Ok(true),
        // `DO NOTHING` inserted none, which `SeaORM` reports client-side; the
        // statement itself succeeded.
        Err(ScopeError::Db(DbErr::RecordNotInserted)) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Write a period row's counter and threshold marker, conditional on
/// `expected_version`. `false` means the row moved first.
///
/// # Errors
///
/// The scope or database error of the update.
pub async fn write_counter(
    runner: &impl DBRunner,
    scope: &AccessScope,
    period_id: Uuid,
    expected_version: i32,
    consumed: i64,
    marker: Option<i16>,
    now: OffsetDateTime,
) -> Result<bool, ScopeError> {
    let affected = Entity::update_many()
        .col_expr(Column::Consumed, consumed.into())
        .col_expr(Column::HighestCrossedThresholdPct, marker.into())
        .col_expr(Column::RecordVersion, (expected_version + 1).into())
        .col_expr(Column::UpdatedAt, now.into())
        .filter(Column::PeriodId.eq(period_id))
        .filter(Column::RecordVersion.eq(expected_version))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(affected.rows_affected == 1)
}

/// Mark one period row settled. The rollover event is enqueued with it, in the
/// same transaction.
///
/// # Errors
///
/// The scope or database error of the update.
pub async fn mark_settled(
    runner: &impl DBRunner,
    scope: &AccessScope,
    period_id: Uuid,
    now: OffsetDateTime,
) -> Result<bool, ScopeError> {
    let affected = Entity::update_many()
        .col_expr(Column::IsSettled, true.into())
        .col_expr(Column::UpdatedAt, now.into())
        .filter(Column::PeriodId.eq(period_id))
        .filter(Column::IsSettled.eq(false))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(affected.rows_affected == 1)
}

/// The allocation counter row of `quota_id`, locked for update.
///
/// Allocation Quotas keep their counter and threshold marker in their own
/// table; the consumption store reads both through one interface so the
/// threshold routine does not branch on the accounting model.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find_allocation_for_update(
    runner: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    wait: RowWait,
) -> Result<Option<crate::infra::storage::entity::quota_allocation_counter::Model>, ScopeError> {
    use crate::infra::storage::entity::quota_allocation_counter as alloc;
    wait.apply(alloc::Entity::find().filter(alloc::Column::QuotaId.eq(quota_id)))
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

/// The allocation counter row, unlocked, for a snapshot read.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find_allocation(
    runner: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
) -> Result<Option<crate::infra::storage::entity::quota_allocation_counter::Model>, ScopeError> {
    use crate::infra::storage::entity::quota_allocation_counter as alloc;
    alloc::Entity::find()
        .filter(alloc::Column::QuotaId.eq(quota_id))
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

/// Write an allocation counter and its threshold marker, conditional on
/// `expected_version`.
///
/// # Errors
///
/// The scope or database error of the update.
pub async fn write_allocation(
    runner: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    expected_version: i32,
    in_flight: i64,
    marker: Option<i16>,
    now: OffsetDateTime,
) -> Result<bool, ScopeError> {
    use crate::infra::storage::entity::quota_allocation_counter as alloc;
    let affected = alloc::Entity::update_many()
        .col_expr(alloc::Column::InFlight, in_flight.into())
        .col_expr(alloc::Column::HighestCrossedThresholdPct, marker.into())
        .col_expr(alloc::Column::RecordVersion, (expected_version + 1).into())
        .col_expr(alloc::Column::UpdatedAt, now.into())
        .filter(alloc::Column::QuotaId.eq(quota_id))
        .filter(alloc::Column::RecordVersion.eq(expected_version))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(affected.rows_affected == 1)
}
