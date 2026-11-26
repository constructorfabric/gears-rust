//! `qe_charges`, `qe_charge_entries` and `qe_charge_returns`: the charge ledger
//! (ADR-0011).
//!
//! A charge is addressed by the scope of its debit or commit and is read and
//! written under that scope's stripe (lock rank 2), so its rows take no lock of
//! their own. A return record is read and written under the stripe of its
//! return key.
//!
//! A charge is deleted, and unknown to every read here, once the replay window
//! has passed since its creation and since the last return or rollback, and no
//! allocation entry holds an amount on an active Quota. The rows store that
//! rule, kept true by every writer of a charge, its entries or a Quota's
//! status:
//!
//! - an entry is `holding` only if its Quota is an allocation Quota and
//!   `returned < charged`; it stops holding when the last of it is returned or
//!   the sweeper releases the holds of its deactivated Quota;
//! - a charge's `expires_at` is `NULL` if and only if one of its entries is
//!   holding;
//! - otherwise `expires_at` is the later of `return_window_end` and
//!   `kept_until`.
//!
//! A deactivated Quota keeps `holding` on its entries until the sweeper
//! releases them, so [`live`] and [`deleted`] also check the Quota of a holding
//! entry. Reads take [`live`], and the sweeper's delete carries [`deleted`], so
//! no reader depends on the sweeper having run.

use quota_enforcement_sdk::QuotaType;
use sea_orm::sea_query::{Asterisk, Expr, ExprTrait as _, Query, SimpleExpr};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, FromQueryResult, IntoActiveModel, Order,
    QueryFilter, QueryOrder, QuerySelect,
};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
    secure_insert_many,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::idempotency_repo::{Inserted, ScopeKey};
use crate::infra::storage::entity::{charge, charge_entry, charge_return, quota};
use crate::infra::storage::quota_mapping::STATUS_ACTIVE;

/// Whether one of this charge's entries is holding.
fn has_holding_entry() -> SimpleExpr {
    Expr::exists(
        Query::select()
            .expr(Expr::val(1))
            .from(charge_entry::Entity)
            .and_where(
                Expr::col((charge_entry::Entity, charge_entry::Column::ChargeId))
                    .equals((charge::Entity, charge::Column::Id)),
            )
            .and_where(Expr::col((charge_entry::Entity, charge_entry::Column::Holding)).eq(true))
            .to_owned(),
    )
}

/// Whether one of this charge's holding entries is on an active Quota.
fn held_on_active_quota() -> SimpleExpr {
    let active = Query::select()
        .expr(Expr::val(1))
        .from(quota::Entity)
        .and_where(
            Expr::col((quota::Entity, quota::Column::Id))
                .equals((charge_entry::Entity, charge_entry::Column::QuotaId)),
        )
        .and_where(Expr::col((quota::Entity, quota::Column::Status)).eq(STATUS_ACTIVE))
        .to_owned();
    Expr::exists(
        Query::select()
            .expr(Expr::val(1))
            .from(charge_entry::Entity)
            .and_where(
                Expr::col((charge_entry::Entity, charge_entry::Column::ChargeId))
                    .equals((charge::Entity, charge::Column::Id)),
            )
            .and_where(Expr::col((charge_entry::Entity, charge_entry::Column::Holding)).eq(true))
            .and_where(Expr::exists(active))
            .to_owned(),
    )
}

/// The deletion rule at `now`.
fn deleted(now: OffsetDateTime) -> Condition {
    Condition::any()
        .add(charge::Column::ExpiresAt.lte(now))
        .add(
            Condition::all()
                .add(charge::Column::ExpiresAt.is_null())
                .add(charge::Column::ReturnWindowEnd.lte(now))
                .add(
                    Condition::any()
                        .add(charge::Column::KeptUntil.is_null())
                        .add(charge::Column::KeptUntil.lte(now)),
                )
                .add(held_on_active_quota().not()),
        )
}

/// Its negation: the charges every operation still knows.
fn live(now: OffsetDateTime) -> Condition {
    Condition::any().add(charge::Column::ExpiresAt.gt(now)).add(
        Condition::all()
            .add(charge::Column::ExpiresAt.is_null())
            .add(
                Condition::any()
                    .add(charge::Column::ReturnWindowEnd.gt(now))
                    .add(charge::Column::KeptUntil.gt(now))
                    .add(held_on_active_quota()),
            ),
    )
}

fn addressed(key: &ScopeKey<'_>) -> Condition {
    Condition::all()
        .add(charge::Column::TenantId.eq(key.tenant_id))
        .add(charge::Column::Operation.eq(key.operation_type))
        .add(charge::Column::IdemKey.eq(key.idem_key))
}

/// The live charge under `key`, if any.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &ScopeKey<'_>,
    now: OffsetDateTime,
) -> Result<Option<charge::Model>, ScopeError> {
    charge::Entity::find()
        .filter(addressed(key))
        .filter(live(now))
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

/// The charge `id`, if it is live.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find_live_by_id(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
    now: OffsetDateTime,
) -> Result<Option<charge::Model>, ScopeError> {
    charge::Entity::find()
        .filter(charge::Column::Id.eq(id))
        .filter(live(now))
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

/// Remove a deleted charge under `key`, so a new debit or commit can take the
/// key. The caller holds the key's stripe.
///
/// # Errors
///
/// The scope or database error of the delete.
pub async fn delete_deleted_at_key(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &ScopeKey<'_>,
    now: OffsetDateTime,
) -> Result<u64, ScopeError> {
    let affected = charge::Entity::delete_many()
        .filter(addressed(key))
        .filter(deleted(now))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(affected.rows_affected)
}

fn keyed(tenant_id: Uuid, operation: &str, keys: &[&str]) -> Condition {
    Condition::all()
        .add(charge::Column::TenantId.eq(tenant_id))
        .add(charge::Column::Operation.eq(operation))
        .add(charge::Column::IdemKey.is_in(keys.iter().copied()))
}

/// Those of `keys` under `(tenant_id, operation)` that hold a live charge. A
/// platform read of existence only: the tenant is fixed by the predicate, and
/// the caller holds the keys' stripes.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn live_keys(
    runner: &impl DBRunner,
    tenant_id: Uuid,
    operation: &str,
    keys: &[&str],
    now: OffsetDateTime,
) -> Result<Vec<String>, ScopeError> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    Ok(charge::Entity::find()
        .filter(keyed(tenant_id, operation, keys))
        .filter(live(now))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(runner)
        .await?
        .into_iter()
        .map(|row| row.idem_key)
        .collect())
}

/// [`delete_deleted_at_key`] for every key of `keys` in one statement. It
/// removes only charges the sweeper would remove, as a platform rule.
///
/// # Errors
///
/// The scope or database error of the delete.
pub async fn delete_deleted_at_keys(
    runner: &impl DBRunner,
    tenant_id: Uuid,
    operation: &str,
    keys: &[&str],
    now: OffsetDateTime,
) -> Result<u64, ScopeError> {
    if keys.is_empty() {
        return Ok(0);
    }
    let affected = charge::Entity::delete_many()
        .filter(keyed(tenant_id, operation, keys))
        .filter(deleted(now))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(runner)
        .await?;
    Ok(affected.rows_affected)
}

/// The entries of the charge `charge_id`, by chain and position.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn entries_of(
    runner: &impl DBRunner,
    scope: &AccessScope,
    charge_id: Uuid,
) -> Result<Vec<charge_entry::Model>, ScopeError> {
    charge_entry::Entity::find()
        .filter(charge_entry::Column::ChargeId.eq(charge_id))
        .order_by(charge_entry::Column::Chain, Order::Asc)
        .order_by(charge_entry::Column::Position, Order::Asc)
        .secure()
        .scope_with(scope)
        .all(runner)
        .await
}

/// Insert a charge and its entries (rank 6), reporting a charge already under
/// the same scope as [`Inserted::Raced`]. The holding flags and `expires_at`
/// are set here from the entries' Quotas.
///
/// # Errors
///
/// The scope or database error of an insert, other than that unique
/// violation.
pub async fn insert(
    runner: &impl DBRunner,
    scope: &AccessScope,
    mut charge: charge::Model,
    mut entries: Vec<charge_entry::Model>,
) -> Result<Inserted, ScopeError> {
    let quota_ids: Vec<Uuid> = entries.iter().map(|entry| entry.quota_id).collect();
    // A platform rule: the Quota's type and status, whoever may read it.
    let allocation: Vec<Uuid> = quota::Entity::find()
        .filter(quota::Column::Id.is_in(quota_ids))
        .filter(quota::Column::QuotaType.eq(QuotaType::Allocation.as_gts_id()))
        .filter(quota::Column::Status.eq(STATUS_ACTIVE))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(runner)
        .await?
        .into_iter()
        .map(|row| row.id)
        .collect();
    settle(&mut charge, &mut entries, |quota_id| {
        allocation.contains(&quota_id)
    });
    let row = charge.into_active_model().reset_all();
    match secure_insert::<charge::Entity>(row, scope, runner).await {
        Ok(_) => {}
        Err(error) if error.is_unique_violation() => return Ok(Inserted::Raced),
        Err(error) => return Err(error),
    }
    for entry in entries {
        let row = entry.into_active_model().reset_all();
        secure_insert::<charge_entry::Entity>(row, scope, runner).await?;
    }
    Ok(Inserted::Yes)
}

/// Set the holding flags of `entries` and the `expires_at` of their `charge`:
/// an entry holds while something of it remains on an active allocation Quota.
fn settle(
    charge: &mut charge::Model,
    entries: &mut [charge_entry::Model],
    allocation: impl Fn(Uuid) -> bool,
) {
    for entry in entries.iter_mut() {
        entry.holding = entry.returned < entry.charged && allocation(entry.quota_id);
    }
    charge.expires_at = if entries.iter().any(|entry| entry.holding) {
        None
    } else {
        Some(charge.kept_until.map_or(charge.return_window_end, |kept| {
            Ord::max(kept, charge.return_window_end)
        }))
    };
}

/// [`insert`] for the charges of one batch, in two statements. `allocation`
/// answers for the Quotas the caller has locked: whether one is an active
/// allocation Quota. A charge already under one of the scopes is
/// [`Inserted::Raced`], and nothing of the batch is kept: the caller rolls
/// its transaction back.
///
/// # Errors
///
/// The scope or database error of an insert, other than that unique
/// violation.
pub async fn insert_all(
    runner: &impl DBRunner,
    scope: &AccessScope,
    charges: Vec<(charge::Model, Vec<charge_entry::Model>)>,
    allocation: impl Fn(Uuid) -> bool,
) -> Result<Inserted, ScopeError> {
    let mut rows = Vec::with_capacity(charges.len());
    let mut entry_rows = Vec::new();
    for (mut charge, mut entries) in charges {
        settle(&mut charge, &mut entries, &allocation);
        rows.push(charge.into_active_model().reset_all());
        entry_rows.extend(
            entries
                .into_iter()
                .map(|entry| entry.into_active_model().reset_all()),
        );
    }
    match secure_insert_many::<charge::Entity>(rows, scope, runner).await {
        Ok(()) => {}
        Err(error) if error.is_unique_violation() => return Ok(Inserted::Raced),
        Err(error) => return Err(error),
    }
    secure_insert_many::<charge_entry::Entity>(entry_rows, scope, runner).await?;
    Ok(Inserted::Yes)
}

/// Add `amount` to the returned amount of one entry, which stops holding when
/// nothing of it remains. The caller then refreshes the charge's
/// [`refresh_expiry`].
///
/// # Errors
///
/// The scope or database error of the update.
pub async fn add_returned(
    runner: &impl DBRunner,
    scope: &AccessScope,
    entry: &charge_entry::Model,
    amount: i64,
) -> Result<(), ScopeError> {
    charge_entry::Entity::update_many()
        .col_expr(
            charge_entry::Column::Returned,
            Expr::col(charge_entry::Column::Returned).add(amount),
        )
        .col_expr(
            charge_entry::Column::Holding,
            Expr::col(charge_entry::Column::Holding).and(
                Expr::col(charge_entry::Column::Returned)
                    .add(amount)
                    .lt(Expr::col(charge_entry::Column::Charged)),
            ),
        )
        .filter(charge_entry::Column::ChargeId.eq(entry.charge_id))
        .filter(charge_entry::Column::Chain.eq(entry.chain))
        .filter(charge_entry::Column::Position.eq(entry.position))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(())
}

/// Keep the charge `charge_id` at least until `until`.
///
/// # Errors
///
/// The scope or database error of the update.
pub async fn keep_until(
    runner: &impl DBRunner,
    scope: &AccessScope,
    charge_id: Uuid,
    until: OffsetDateTime,
) -> Result<(), ScopeError> {
    charge::Entity::update_many()
        .col_expr(charge::Column::KeptUntil, Expr::value(until))
        .filter(charge::Column::Id.eq(charge_id))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(())
}

/// Set the `expires_at` of the charges `charge_ids` from their entries and
/// windows.
///
/// # Errors
///
/// The scope or database error of the update.
pub async fn refresh_expiry(
    runner: &impl DBRunner,
    scope: &AccessScope,
    charge_ids: &[Uuid],
) -> Result<(), ScopeError> {
    let later = Expr::case(
        Expr::col(charge::Column::KeptUntil).gt(Expr::col(charge::Column::ReturnWindowEnd)),
        Expr::col(charge::Column::KeptUntil),
    )
    .finally(Expr::col(charge::Column::ReturnWindowEnd));
    charge::Entity::update_many()
        .col_expr(
            charge::Column::ExpiresAt,
            Expr::case(has_holding_entry(), Expr::null())
                .finally(later)
                .into(),
        )
        .filter(charge::Column::Id.is_in(charge_ids.iter().copied()))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(())
}

/// The return record under `(tenant_id, return_key)`, whatever its charge.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find_return(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    return_key: &str,
) -> Result<Option<charge_return::Model>, ScopeError> {
    charge_return::Entity::find()
        .filter(charge_return::Column::TenantId.eq(tenant_id))
        .filter(charge_return::Column::ReturnKey.eq(return_key))
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

/// Remove the return record under `(tenant_id, return_key)`: one whose charge
/// is deleted, and so deleted with it.
///
/// # Errors
///
/// The scope or database error of the delete.
pub async fn delete_return(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    return_key: &str,
) -> Result<(), ScopeError> {
    charge_return::Entity::delete_many()
        .filter(charge_return::Column::TenantId.eq(tenant_id))
        .filter(charge_return::Column::ReturnKey.eq(return_key))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(())
}

/// Insert a return record (rank 6).
///
/// # Errors
///
/// The scope or database error of the insert.
pub async fn insert_return(
    runner: &impl DBRunner,
    scope: &AccessScope,
    record: charge_return::Model,
) -> Result<(), ScopeError> {
    let row = record.into_active_model().reset_all();
    secure_insert::<charge_return::Entity>(row, scope, runner).await?;
    Ok(())
}

/// A charge's id and the scope whose stripe guards it: tenant, operation,
/// idempotency key.
pub type ChargeScope = (Uuid, Uuid, String, String);

fn scope_of(row: charge::Model) -> ChargeScope {
    (row.id, row.tenant_id, row.operation, row.idem_key)
}

/// Up to `batch_size` charges whose `expires_at` has passed at `now`, the
/// earliest first. A held charge is not among them, nor reached on the way.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn select_expired(
    runner: &impl DBRunner,
    scope: &AccessScope,
    batch_size: u32,
    now: OffsetDateTime,
) -> Result<Vec<ChargeScope>, ScopeError> {
    Ok(charge::Entity::find()
        .filter(charge::Column::ExpiresAt.lte(now))
        .order_by(charge::Column::ExpiresAt, Order::Asc)
        .limit(u64::from(batch_size))
        .secure()
        .scope_with(scope)
        .all(runner)
        .await?
        .into_iter()
        .map(scope_of)
        .collect())
}

/// The charge `id`, live or deleted.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find_by_id(
    runner: &impl DBRunner,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Option<charge::Model>, ScopeError> {
    charge::Entity::find()
        .filter(charge::Column::Id.eq(id))
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}

/// Up to `limit` deactivated Quotas whose holds the sweeper has not released.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn quotas_with_pending_holds(
    runner: &impl DBRunner,
    scope: &AccessScope,
    limit: u32,
) -> Result<Vec<Uuid>, ScopeError> {
    Ok(quota::Entity::find()
        .filter(quota::Column::HoldsPending.eq(true))
        .limit(u64::from(limit))
        .secure()
        .scope_with(scope)
        .all(runner)
        .await?
        .into_iter()
        .map(|row| row.id)
        .collect())
}

/// The charges of up to `limit` holding entries on the Quota `quota_id`, each
/// once.
///
/// # Errors
///
/// The scope or database error of a read.
pub async fn charges_held_on(
    runner: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    limit: u32,
) -> Result<Vec<ChargeScope>, ScopeError> {
    let mut charges: Vec<Uuid> = charge_entry::Entity::find()
        .filter(charge_entry::Column::QuotaId.eq(quota_id))
        .filter(charge_entry::Column::Holding.eq(true))
        .limit(u64::from(limit))
        .secure()
        .scope_with(scope)
        .all(runner)
        .await?
        .into_iter()
        .map(|row| row.charge_id)
        .collect();
    charges.sort_unstable();
    charges.dedup();
    if charges.is_empty() {
        return Ok(Vec::new());
    }
    Ok(charge::Entity::find()
        .filter(charge::Column::Id.is_in(charges))
        .secure()
        .scope_with(scope)
        .all(runner)
        .await?
        .into_iter()
        .map(scope_of)
        .collect())
}

/// Stop the entries of the charges `charge_ids` on the Quota `quota_id` from
/// holding, and refresh the charges' `expires_at`.
///
/// # Errors
///
/// The scope or database error of an update.
pub async fn release_holds(
    runner: &impl DBRunner,
    scope: &AccessScope,
    charge_ids: &[Uuid],
    quota_id: Uuid,
) -> Result<(), ScopeError> {
    if charge_ids.is_empty() {
        return Ok(());
    }
    charge_entry::Entity::update_many()
        .col_expr(charge_entry::Column::Holding, Expr::value(false))
        .filter(charge_entry::Column::ChargeId.is_in(charge_ids.iter().copied()))
        .filter(charge_entry::Column::QuotaId.eq(quota_id))
        .filter(charge_entry::Column::Holding.eq(true))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    refresh_expiry(runner, scope, charge_ids).await
}

/// Clear the Quota `quota_id`'s `holds_pending` once none of its entries is
/// holding. No entry starts holding on a deactivated Quota, so the flag stays
/// clear.
///
/// # Errors
///
/// The scope or database error of the update.
pub async fn clear_pending_holds(
    runner: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
) -> Result<(), ScopeError> {
    let holding = Query::select()
        .expr(Expr::val(1))
        .from(charge_entry::Entity)
        .and_where(
            Expr::col((charge_entry::Entity, charge_entry::Column::QuotaId))
                .equals((quota::Entity, quota::Column::Id)),
        )
        .and_where(Expr::col((charge_entry::Entity, charge_entry::Column::Holding)).eq(true))
        .to_owned();
    quota::Entity::update_many()
        .col_expr(quota::Column::HoldsPending, Expr::value(false))
        .filter(quota::Column::Id.eq(quota_id))
        .filter(Expr::exists(holding).not())
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(())
}

/// Remove those of the charges `ids` that are still deleted at `now`; their
/// entries and return records go with them. The rule is part of the delete: a
/// return may have kept a charge since it was selected.
///
/// # Errors
///
/// The scope or database error of the delete.
pub async fn delete_if_deleted(
    runner: &impl DBRunner,
    scope: &AccessScope,
    ids: &[Uuid],
    now: OffsetDateTime,
) -> Result<u64, ScopeError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let affected = charge::Entity::delete_many()
        .filter(charge::Column::Id.is_in(ids.iter().copied()))
        .filter(deleted(now))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(affected.rows_affected)
}

/// One metric's held allocation charges.
#[derive(Debug, FromQueryResult)]
struct HeldRow {
    metric: String,
    total: i64,
    oldest: OffsetDateTime,
}

/// Per metric, how many charges whose return window ended before `before`
/// still hold an amount, and when the oldest was created. `expires_at` is null
/// exactly while an entry holds, so the read stays on the index of held
/// charges. A hold on a deactivated Quota counts until the sweeper releases
/// it.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn held_allocation_by_metric(
    runner: &impl DBRunner,
    scope: &AccessScope,
    before: OffsetDateTime,
) -> Result<Vec<(String, u64, OffsetDateTime)>, ScopeError> {
    let rows = charge::Entity::find()
        .filter(charge::Column::ExpiresAt.is_null())
        .filter(charge::Column::ReturnWindowEnd.lte(before))
        .secure()
        .scope_with(scope)
        .project_all(runner, |q| {
            q.select_only()
                .column(charge::Column::Metric)
                .column_as(Expr::col(Asterisk).count(), "total")
                .column_as(Expr::col(charge::Column::CreatedAt).min(), "oldest")
                .group_by(charge::Column::Metric)
                .into_model::<HeldRow>()
        })
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.metric,
                u64::try_from(row.total).unwrap_or(0),
                row.oldest,
            )
        })
        .collect())
}
