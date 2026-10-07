//! Live reservations, confirmation and retained release history.
use super::{HeadWrite, driver_failure, map_unique};
use crate::domain::references::{RefKind, ReferenceSummary};
use crate::infra::storage::{RepoError, entity::sku_reference};
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, QuerySelect, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use uuid::Uuid;
/// Stored reservation, including its release history and actor attribution.
pub type SkuReference = sku_reference::Model;
/// Confirmation distinguishes replay, tombstone and absent reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmOutcome {
    Confirmed,
    AlreadyConfirmed,
    Released,
    Missing,
}
fn key(tenant: Uuid, id: Uuid) -> Condition {
    Condition::all()
        .add(sku_reference::Column::TenantId.eq(tenant))
        .add(sku_reference::Column::Id.eq(id))
}
/// Look up a live logical reference before an idempotent reserve.
/// # Errors
/// Returns scoped storage failures.
pub async fn find_live_reference(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    owner: &str,
    kind: RefKind,
    ref_id: Uuid,
) -> Result<Option<SkuReference>, RepoError> {
    sku_reference::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(sku_reference::Column::TenantId.eq(tenant_id))
                .add(sku_reference::Column::OwnerGear.eq(owner))
                .add(sku_reference::Column::RefKind.eq(kind.as_str()))
                .add(sku_reference::Column::RefId.eq(ref_id))
                .add(sku_reference::Column::State.ne("released")),
        )
        .one(runner)
        .await
        .map_err(|e| driver_failure("find live reference".into(), e))
}
/// Insert a fresh attempt after the door's eligibility check in the same serializable transaction.
/// # Errors
/// Returns `REFERENCE_EXISTS` or scoped storage failures.
#[expect(
    clippy::too_many_arguments,
    reason = "the reservation's scoped key, its logical reference and its attribution stay explicit"
)]
pub async fn reserve_reference(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    sku_id: Uuid,
    owner: &str,
    kind: RefKind,
    ref_id: Uuid,
    actor: Uuid,
    now: OffsetDateTime,
) -> Result<SkuReference, RepoError> {
    let model = sku_reference::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        sku_id: Set(sku_id),
        owner_gear: Set(owner.into()),
        ref_kind: Set(kind.as_str().into()),
        ref_id: Set(ref_id),
        state: Set("reserved".into()),
        reserved_by: Set(actor),
        reserved_at: Set(now),
        confirmed_at: Set(None),
        released_at: Set(None),
        released_by: Set(None),
        release_reason: Set(None),
        forced: Set(false),
    };
    sku_reference::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure("reference scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map_err(|e| map_unique("reserve reference".into(), e))
}
/// Confirm only a reserved attempt; a released attempt can never be reactivated.
/// # Errors
/// Returns scoped storage or corrupt-state failures.
pub async fn confirm_reference(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    now: OffsetDateTime,
) -> Result<ConfirmOutcome, RepoError> {
    let r = sku_reference::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(sku_reference::Column::State, Expr::value("confirmed"))
        .col_expr(sku_reference::Column::ConfirmedAt, Expr::value(now))
        .filter(key(tenant_id, id).add(sku_reference::Column::State.eq("reserved")))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("confirm reference".into(), e))?;
    if r.rows_affected == 1 {
        return Ok(ConfirmOutcome::Confirmed);
    }
    match find_reference(runner, scope, tenant_id, id).await? {
        None => Ok(ConfirmOutcome::Missing),
        Some(row) => match row.state.as_str() {
            "confirmed" => Ok(ConfirmOutcome::AlreadyConfirmed),
            "released" => Ok(ConfirmOutcome::Released),
            other => Err(RepoError::CorruptRow(format!(
                "unmatched reference state {other}"
            ))),
        },
    }
}
/// Read an attempt, including a released tombstone, for owner authorization/replay.
/// # Errors
/// Returns scoped storage failures.
pub async fn find_reference(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Option<SkuReference>, RepoError> {
    sku_reference::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(key(tenant_id, id))
        .one(runner)
        .await
        .map_err(|e| driver_failure("find reference".into(), e))
}
/// The attempts of `ids` the tenant holds, released tombstones included, in ONE read (RS-13); an
/// id the tenant does not hold is absent.
/// # Errors
/// Returns scoped storage failures.
pub async fn find_references(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    ids: &[Uuid],
) -> Result<Vec<SkuReference>, RepoError> {
    sku_reference::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(sku_reference::Column::TenantId.eq(tenant_id))
                .add(sku_reference::Column::Id.is_in(ids.iter().copied())),
        )
        .all(runner)
        .await
        .map_err(|e| driver_failure("find references".into(), e))
}
/// Release once; retries leave the original attribution unchanged.
/// # Errors
/// Returns scoped storage failures.
#[expect(
    clippy::too_many_arguments,
    reason = "the release keeps its scoped key and its attribution explicit"
)]
pub async fn release_reference(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    actor: Uuid,
    reason: Option<&str>,
    forced: bool,
    now: OffsetDateTime,
) -> Result<HeadWrite<SkuReference>, RepoError> {
    let r = sku_reference::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(sku_reference::Column::State, Expr::value("released"))
        .col_expr(sku_reference::Column::ReleasedAt, Expr::value(now))
        .col_expr(sku_reference::Column::ReleasedBy, Expr::value(actor))
        .col_expr(sku_reference::Column::ReleaseReason, Expr::value(reason))
        .col_expr(sku_reference::Column::Forced, Expr::value(forced))
        .filter(key(tenant_id, id).add(sku_reference::Column::State.ne("released")))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("release reference".into(), e))?;
    if r.rows_affected == 0 {
        return Ok(HeadWrite::Unmatched);
    }
    find_reference(runner, scope, tenant_id, id)
        .await?
        .map(HeadWrite::Written)
        .ok_or_else(|| RepoError::CorruptRow("released reference disappeared".into()))
}
/// List every live attempt, including reservations that have not been confirmed.
/// # Errors
/// Returns scoped storage failures.
pub async fn live_references(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    sku_id: Uuid,
) -> Result<Vec<SkuReference>, RepoError> {
    list_references(runner, scope, tenant_id, sku_id, false).await
}
/// List attempts, optionally including released history; tenant and SKU scope always apply.
/// # Errors
/// Returns scoped storage failures.
pub async fn list_references(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    sku_id: Uuid,
    include_released: bool,
) -> Result<Vec<SkuReference>, RepoError> {
    let mut predicate = Condition::all()
        .add(sku_reference::Column::TenantId.eq(tenant_id))
        .add(sku_reference::Column::SkuId.eq(sku_id));
    if !include_released {
        predicate = predicate.add(sku_reference::Column::State.ne("released"));
    }
    sku_reference::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(predicate)
        .order_by(sku_reference::Column::Id, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list references".into(), e))
}
#[derive(Debug, sea_orm::FromQueryResult)]
struct ReferenceCount {
    owner_gear: String,
    ref_kind: String,
    state: String,
    n: i64,
}
/// Summarize live price book entries/plans and the reserved subset, in ONE grouped count by
/// `(owner_gear, ref_kind, state)` (RS-15): a SKU card reads a handful of rows, however many
/// entries name the SKU.
/// # Errors
/// Returns scoped storage failures, an unknown stored reference kind or a negative count.
pub async fn reference_summary(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    sku_id: Uuid,
) -> Result<ReferenceSummary, RepoError> {
    let counts = sku_reference::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(sku_reference::Column::TenantId.eq(tenant_id))
                .add(sku_reference::Column::SkuId.eq(sku_id))
                .add(sku_reference::Column::State.ne("released")),
        )
        .project_all(runner, |q| {
            q.select_only()
                .column(sku_reference::Column::OwnerGear)
                .column(sku_reference::Column::RefKind)
                .column(sku_reference::Column::State)
                .column_as(
                    Expr::col((sku_reference::Entity, sku_reference::Column::Id)).count(),
                    "n",
                )
                .group_by(sku_reference::Column::OwnerGear)
                .group_by(sku_reference::Column::RefKind)
                .group_by(sku_reference::Column::State)
                .into_model::<ReferenceCount>()
        })
        .await
        .map_err(|e| driver_failure("count references".into(), e))?;
    let mut summary = ReferenceSummary::default();
    for r in counts {
        let n = u32::try_from(r.n)
            .map_err(|_| RepoError::CorruptRow(format!("a reference count {}", r.n)))?;
        match r.ref_kind.as_str() {
            "price_book_entry" => {
                summary.price_book_entries = summary.price_book_entries.saturating_add(n);
            }
            "plan_item" | "sold_as" => summary.plans = summary.plans.saturating_add(n),
            _ => {
                return Err(RepoError::CorruptRow(format!(
                    "reference kind {}",
                    r.ref_kind
                )));
            }
        }
        let owner = summary.by_owner.entry(r.owner_gear).or_default();
        let kind = owner.entry(r.ref_kind).or_default();
        *kind = kind.saturating_add(n);
        if r.state == "reserved" {
            let reserved = owner.entry("reserved".into()).or_default();
            *reserved = reserved.saturating_add(n);
            summary.reserved = summary.reserved.saturating_add(n);
        }
    }
    Ok(summary)
}
