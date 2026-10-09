//! `qe_leases`, `qe_lease_holds`, and `qe_lease_capacity_counters`.
//!
//! Lock rank (ADR-0002, extended to every row kind this plugin takes): Quota
//! rows, then idempotency records, then lease rows, then capacity rows, then
//! counter rows, then hold rows under their counter. Every function here says
//! which rank it belongs to, because a caller that takes them out of order can
//! deadlock against one that does not.
//!
//! Holds are immutable once written, apart from `returned_at`. That is what
//! lets a settlement read them unlocked to learn the metric and the plan, and
//! re-verify under the lock, instead of locking the lease before it knows
//! whether it may.

use sea_orm::sea_query::{LockBehavior, LockType};

pub use super::RowWait;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveValue, ColumnTrait, DbErr, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect,
};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt, secure_insert,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::infra::storage::entity::{lease, lease_capacity_counter, lease_hold};

/// Wire value of [`quota_enforcement_sdk::LeaseState::Active`]. The only
/// non-terminal state, and the one every guard filters on.
pub const STATE_ACTIVE: &str = "active";
/// Wire value for a lease converted into a debit.
pub const STATE_COMMITTED: &str = "committed";
/// Wire value for a lease whose holder gave the capacity back.
pub const STATE_RELEASED: &str = "released";
/// Wire value for a lease the sweeper reclaimed after its TTL.
pub const STATE_AUTO_RELEASED: &str = "auto_released";
/// Wire value for a lease resolved by its Quota's deactivation.
pub const STATE_RESOLVED_BY_DEACTIVATION: &str = "resolved_by_deactivation";

/// Values of a new lease row.
pub struct NewLease<'a> {
    /// The generated token.
    pub token: Uuid,
    /// Owning tenant.
    pub tenant_id: Uuid,
    /// The metric the hold is on.
    pub metric: &'a str,
    /// The acquisition's idempotency subject key.
    pub subject_key: &'a [u8],
    /// The attribution the acquisition was authorized under.
    pub attribution_hash: &'a [u8],
    /// The acquisition's own idempotency key.
    pub idem_key: &'a str,
    /// What the caller reserved.
    pub reserved_amount: i64,
    /// Acquisition time, the transaction's single instant.
    pub now: OffsetDateTime,
    /// When the hold lapses.
    pub expiry_at: OffsetDateTime,
}

/// Insert the lease row (rank 6).
///
/// # Errors
///
/// The scope or database error of the insert.
pub async fn insert_lease(
    runner: &impl DBRunner,
    scope: &AccessScope,
    lease: &NewLease<'_>,
) -> Result<lease::Model, ScopeError> {
    secure_insert::<lease::Entity>(
        lease::ActiveModel {
            token: ActiveValue::Set(lease.token),
            tenant_id: ActiveValue::Set(lease.tenant_id),
            metric: ActiveValue::Set(lease.metric.to_owned()),
            subject_key: ActiveValue::Set(lease.subject_key.to_vec()),
            attribution_hash: ActiveValue::Set(lease.attribution_hash.to_vec()),
            idem_key: ActiveValue::Set(lease.idem_key.to_owned()),
            state: ActiveValue::Set(STATE_ACTIVE.to_owned()),
            reserved_amount: ActiveValue::Set(lease.reserved_amount),
            acquired_at: ActiveValue::Set(lease.now),
            expiry_at: ActiveValue::Set(lease.expiry_at),
            resolved_at: ActiveValue::Set(None),
            record_version: ActiveValue::Set(1),
            created_at: ActiveValue::Set(lease.now),
            updated_at: ActiveValue::Set(lease.now),
        },
        scope,
        runner,
    )
    .await
}

/// Insert one hold (rank 6).
///
/// # Errors
///
/// The scope or database error of the insert.
pub async fn insert_hold(
    runner: &impl DBRunner,
    scope: &AccessScope,
    token: Uuid,
    tenant_id: Uuid,
    quota_id: Uuid,
    held_amount: i64,
    period_id: Option<Uuid>,
) -> Result<(), ScopeError> {
    secure_insert::<lease_hold::Entity>(
        lease_hold::ActiveModel {
            lease_token: ActiveValue::Set(token),
            quota_id: ActiveValue::Set(quota_id),
            tenant_id: ActiveValue::Set(tenant_id),
            held_amount: ActiveValue::Set(held_amount),
            period_id: ActiveValue::Set(period_id),
            returned_at: ActiveValue::Set(None),
        },
        scope,
        runner,
    )
    .await?;
    Ok(())
}

/// One lease by token, unlocked (rank-free). A settlement reads this before
/// any lock, to learn the metric its contention budget is configured by and
/// the holds it will need, then re-verifies under the lock.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find(
    runner: &impl DBRunner,
    scope: &AccessScope,
    token: Uuid,
) -> Result<Option<lease::Model>, ScopeError> {
    lease::Entity::find_by_id(token)
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

/// One lease by token, locked for update (rank 3).
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find_for_update(
    runner: &impl DBRunner,
    scope: &AccessScope,
    token: Uuid,
    wait: RowWait,
) -> Result<Option<lease::Model>, ScopeError> {
    wait.apply(lease::Entity::find_by_id(token))
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

/// A lease's holds, ascending by `quota_id` so a caller locks their counter
/// rows in the rank-5 order.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn holds_of(
    runner: &impl DBRunner,
    scope: &AccessScope,
    token: Uuid,
) -> Result<Vec<lease_hold::Model>, ScopeError> {
    lease_hold::Entity::find()
        .filter(lease_hold::Column::LeaseToken.eq(token))
        .order_by(lease_hold::Column::QuotaId, Order::Asc)
        .secure()
        .scope_with(scope)
        .all(runner)
        .await
}

/// Move a lease to a terminal state, conditional on it still being active.
/// `false` means another transaction resolved it first.
///
/// # Errors
///
/// The scope or database error of the update.
pub async fn mark_state(
    runner: &impl DBRunner,
    scope: &AccessScope,
    token: Uuid,
    state: &str,
    now: OffsetDateTime,
) -> Result<bool, ScopeError> {
    let affected = lease::Entity::update_many()
        .col_expr(lease::Column::State, state.into())
        .col_expr(lease::Column::ResolvedAt, now.into())
        .col_expr(lease::Column::UpdatedAt, now.into())
        .filter(lease::Column::Token.eq(token))
        .filter(lease::Column::State.eq(STATE_ACTIVE))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(affected.rows_affected == 1)
}

/// Stamp a hold returned, conditional on it not being returned already.
/// `false` means someone else gave this capacity back first, and the caller
/// must not move the counter for it (I4).
///
/// # Errors
///
/// The scope or database error of the update.
pub async fn mark_hold_returned(
    runner: &impl DBRunner,
    scope: &AccessScope,
    token: Uuid,
    quota_id: Uuid,
    now: OffsetDateTime,
) -> Result<bool, ScopeError> {
    let affected = lease_hold::Entity::update_many()
        .col_expr(lease_hold::Column::ReturnedAt, now.into())
        .filter(lease_hold::Column::LeaseToken.eq(token))
        .filter(lease_hold::Column::QuotaId.eq(quota_id))
        .filter(lease_hold::Column::ReturnedAt.is_null())
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(affected.rows_affected == 1)
}

/// How many leases on `(tenant, metric)` are live at `now`: active, and not
/// yet past their expiry.
///
/// This is what admits an acquisition, rather than the maintained
/// `active_count`, so an expired lease frees the cap the instant its TTL
/// passes and without any sweep (I4, I7).
///
/// # Errors
///
/// The scope or database error of the count.
pub async fn count_live(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    metric: &str,
    now: OffsetDateTime,
) -> Result<u64, ScopeError> {
    lease::Entity::find()
        .filter(lease::Column::TenantId.eq(tenant_id))
        .filter(lease::Column::Metric.eq(metric))
        .filter(lease::Column::State.eq(STATE_ACTIVE))
        .filter(lease::Column::ExpiryAt.gt(now))
        .secure()
        .scope_with(scope)
        .count(runner)
        .await
}

/// Every hold on `(quota_id, period_id)` whose lease has expired and whose
/// capacity nobody has returned yet.
///
/// The caller holds the counter row, so what this reports is what that row
/// still owes. Readers, which may not write, subtract the same sum.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn unreturned_expired_holds(
    runner: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    period_id: Option<Uuid>,
    now: OffsetDateTime,
) -> Result<Vec<lease_hold::Model>, ScopeError> {
    let expired = lease::Entity::find()
        .filter(lease::Column::State.eq(STATE_ACTIVE))
        .filter(lease::Column::ExpiryAt.lte(now))
        .secure()
        .scope_with(scope)
        .all(runner)
        .await?;
    if expired.is_empty() {
        return Ok(Vec::new());
    }
    let tokens: Vec<Uuid> = expired.into_iter().map(|lease| lease.token).collect();
    let mut query = lease_hold::Entity::find()
        .filter(lease_hold::Column::QuotaId.eq(quota_id))
        .filter(lease_hold::Column::LeaseToken.is_in(tokens))
        .filter(lease_hold::Column::ReturnedAt.is_null());
    query = match period_id {
        Some(period_id) => query.filter(lease_hold::Column::PeriodId.eq(period_id)),
        None => query.filter(lease_hold::Column::PeriodId.is_null()),
    };
    query
        .order_by(lease_hold::Column::LeaseToken, Order::Asc)
        .secure()
        .scope_with(scope)
        .all(runner)
        .await
}

/// Active leases holding `quota_id` that have not expired, locked in token
/// order (rank 3). The deactivation cascade resolves exactly these; expired
/// ones are already released (I4) and belong to the sweeper.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn active_on_quota_for_update(
    runner: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    now: OffsetDateTime,
    wait: RowWait,
) -> Result<Vec<lease::Model>, ScopeError> {
    let holds = lease_hold::Entity::find()
        .filter(lease_hold::Column::QuotaId.eq(quota_id))
        .filter(lease_hold::Column::ReturnedAt.is_null())
        .secure()
        .scope_with(scope)
        .all(runner)
        .await?;
    if holds.is_empty() {
        return Ok(Vec::new());
    }
    let tokens: Vec<Uuid> = holds.into_iter().map(|hold| hold.lease_token).collect();
    let query = lease::Entity::find()
        .filter(lease::Column::Token.is_in(tokens))
        .filter(lease::Column::State.eq(STATE_ACTIVE))
        .filter(lease::Column::ExpiryAt.gt(now))
        .order_by(lease::Column::Token, Order::Asc);
    let query = wait.apply(query);
    query.secure().scope_with(scope).all(runner).await
}

/// Up to `batch_size` leases expired at or before `before`, locked (rank 3).
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn expired_for_update(
    runner: &impl DBRunner,
    scope: &AccessScope,
    batch_size: u32,
    before: OffsetDateTime,
) -> Result<Vec<lease::Model>, ScopeError> {
    lease::Entity::find()
        .filter(lease::Column::State.eq(STATE_ACTIVE))
        .filter(lease::Column::ExpiryAt.lte(before))
        .order_by(lease::Column::ExpiryAt, Order::Asc)
        .limit(u64::from(batch_size))
        // Leader election is advisory, so two sweep bodies can overlap; each
        // takes only the rows the other is not holding and they partition the
        // batch instead of blocking on one another (ADR-0006).
        .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
        .secure()
        .scope_with(scope)
        .all(runner)
        .await
}

/// Expired, unreclaimed leases by metric, behind the backlog gauge.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn expired_by_metric(
    runner: &impl DBRunner,
    scope: &AccessScope,
    before: OffsetDateTime,
) -> Result<Vec<(String, u64)>, ScopeError> {
    let expired = lease::Entity::find()
        .filter(lease::Column::State.eq(STATE_ACTIVE))
        .filter(lease::Column::ExpiryAt.lte(before))
        .secure()
        .scope_with(scope)
        .all(runner)
        .await?;
    let mut by_metric: Vec<(String, u64)> = Vec::new();
    for lease in expired {
        match by_metric.iter_mut().find(|(m, _)| *m == lease.metric) {
            Some((_, count)) => *count += 1,
            None => by_metric.push((lease.metric, 1)),
        }
    }
    Ok(by_metric)
}

/// Make sure the capacity row of `(tenant, metric)` exists.
///
/// Called with the pair's first Quota, not its first lease, so an acquisition
/// only ever *locks* the row. That matters for the contention budget (I8): an
/// insert that meets another transaction's uncommitted key waits for it with no
/// `NOWAIT` to refuse, while a lock can be refused and retried.
///
/// `ON CONFLICT DO NOTHING` rather than "insert, then tolerate the unique
/// violation": on `PostgreSQL` a failed insert aborts the whole transaction, so
/// nothing after it could run.
///
/// # Errors
///
/// The scope or database error of the insert.
pub async fn ensure_capacity_row(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    metric: &str,
    now: OffsetDateTime,
) -> Result<(), ScopeError> {
    let row = lease_capacity_counter::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        metric: ActiveValue::Set(metric.to_owned()),
        active_count: ActiveValue::Set(0),
        record_version: ActiveValue::Set(1),
        updated_at: ActiveValue::Set(now),
    };
    let keep_existing = OnConflict::columns([
        lease_capacity_counter::Column::TenantId,
        lease_capacity_counter::Column::Metric,
    ])
    .do_nothing()
    .to_owned();
    let inserted = lease_capacity_counter::Entity::insert(row.clone())
        .secure()
        .scope_with_model(scope, &row)?
        // Nothing is updated on conflict, so the tenant cannot change.
        .on_conflict_raw(keep_existing)
        .exec(runner)
        .await;
    match inserted {
        // The row already existed: `DO NOTHING` inserted none, which `SeaORM`
        // reports client-side; the statement itself succeeded.
        Ok(_) | Err(ScopeError::Db(DbErr::RecordNotInserted)) => Ok(()),
        Err(error) => Err(error),
    }
}

/// The capacity row of `(tenant, metric)`, locked (rank 4).
///
/// It exists from the pair's first Quota ([`ensure_capacity_row`]); an
/// acquisition can only reach a pair that has an applicable Quota, so a missing
/// row is an inconsistency and is reported as such rather than created here,
/// where the insert could wait outside the contention budget.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn lock_capacity_row(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    metric: &str,
    wait: RowWait,
) -> Result<Option<lease_capacity_counter::Model>, ScopeError> {
    wait.apply(lease_capacity_counter::Entity::find_by_id((
        tenant_id,
        metric.to_owned(),
    )))
    .secure()
    .scope_with(scope)
    .one(runner)
    .await
}

/// Move the diagnostic active-lease count by `delta`, flooring at zero.
///
/// # Errors
///
/// The scope or database error of the update.
pub async fn bump_active_count(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    metric: &str,
    current: i32,
    delta: i32,
    now: OffsetDateTime,
) -> Result<(), ScopeError> {
    let next = current.saturating_add(delta).max(0);
    lease_capacity_counter::Entity::update_many()
        .col_expr(lease_capacity_counter::Column::ActiveCount, next.into())
        .col_expr(lease_capacity_counter::Column::UpdatedAt, now.into())
        .filter(lease_capacity_counter::Column::TenantId.eq(tenant_id))
        .filter(lease_capacity_counter::Column::Metric.eq(metric))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(())
}

/// Whether any live lease still holds capacity against `period_id`.
///
/// Live means active and not past its expiry: an expired lease is released
/// (I4) and cannot move the period again, so it does not hold settlement open.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn has_live_holds(
    runner: &impl DBRunner,
    scope: &AccessScope,
    period_id: Uuid,
    now: OffsetDateTime,
) -> Result<bool, ScopeError> {
    let holds = lease_hold::Entity::find()
        .filter(lease_hold::Column::PeriodId.eq(period_id))
        .filter(lease_hold::Column::ReturnedAt.is_null())
        .secure()
        .scope_with(scope)
        .all(runner)
        .await?;
    if holds.is_empty() {
        return Ok(false);
    }
    let tokens: Vec<Uuid> = holds.into_iter().map(|hold| hold.lease_token).collect();
    let live = lease::Entity::find()
        .filter(lease::Column::Token.is_in(tokens))
        .filter(lease::Column::State.eq(STATE_ACTIVE))
        .filter(lease::Column::ExpiryAt.gt(now))
        .secure()
        .scope_with(scope)
        .count(runner)
        .await?;
    Ok(live > 0)
}
