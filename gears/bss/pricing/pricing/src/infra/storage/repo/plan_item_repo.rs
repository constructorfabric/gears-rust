//! Scoped plan item persistence with conditional versions (D-407, D-413).
use super::{driver_failure, map_unique, matched};
use crate::domain::plan::{ReferenceState, RevisionState};
use crate::infra::storage::{
    RepoError,
    entity::{plan_item as e, plan_revision},
};
use sea_orm::sea_query::{Expr, ExprTrait, Query, SelectStatement};
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, Set};
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use uuid::Uuid;
fn key(tenant: Uuid, id: Uuid) -> Condition {
    Condition::all()
        .add(e::Column::TenantId.eq(tenant))
        .add(e::Column::Id.eq(id))
}
/// The tenant's unlocked draft revisions: an item changes only while its revision is one.
fn unlocked_drafts(tenant: Uuid) -> SelectStatement {
    Query::select()
        .column(plan_revision::Column::Id)
        .from(plan_revision::Entity)
        .and_where(plan_revision::Column::TenantId.eq(tenant))
        .and_where(plan_revision::Column::State.eq(RevisionState::Draft.as_str()))
        .and_where(plan_revision::Column::PendingUnitId.is_null())
        .to_owned()
}
async fn entry_in_tenant(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    entry: Option<Uuid>,
) -> Result<(), RepoError> {
    if let Some(entry) = entry
        && super::price_book_entry_repo::find(runner, scope, tenant, entry)
            .await?
            .is_none()
    {
        return Err(RepoError::Conflict {
            code: "ENTRY_NOT_FOUND",
        });
    }
    Ok(())
}
/// `m` in the row shape every write stores from D-467 on: the treatment `plan::stored_treatment`
/// derives from its entry (`paid`, or `included` for an item with no entry — a draft waiting for
/// one, D-512, or a copy of a legacy item — the entry-less row the column's CHECK admits) and no
/// quantity, whatever `m` carries.
fn d467_shape(mut m: e::Model) -> e::Model {
    m.treatment = crate::domain::plan::stored_treatment(m.price_book_entry_id)
        .as_str()
        .into();
    m.included_qty = None;
    m.qty_min = None;
    m
}
/// Insert an item into an unlocked draft revision of the tenant, in D-467's row shape whatever
/// `m` carries (`d467_shape`): every writer of a plan item stores it, so none has to repeat it.
/// The revision and the item's entry are re-read here, in the caller's transaction, so a racing
/// submit, book change or delete orders against this write (D-407): the entry must be an entry
/// of the revision's book for the item's own SKU. Answers the row as stored.
/// # Errors
/// `REVISION_NOT_FOUND`, `REVISION_NOT_DRAFT`, `ENTRY_NOT_FOUND`, `ITEM_BOOK_FOREIGN`,
/// `ITEM_ENTRY_SKU_MISMATCH` or `ITEM_SKU_TAKEN`; database failures keep their type.
pub async fn insert(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<e::Model, RepoError> {
    insert_row(runner, scope, d467_shape(m)).await
}
/// [`insert`] of `m` exactly as given, its treatment and quantities included: the row shape of
/// before D-467. No door writes through it; it seeds the legacy rows the suites read, and the
/// tests of the column CHECKs.
/// # Errors
/// As [`insert`]; a shape the column CHECKs refuse is a database failure.
#[cfg(any(test, feature = "test-support"))]
pub async fn insert_as_given(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<e::Model, RepoError> {
    insert_row(runner, scope, m).await
}
async fn insert_row(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<e::Model, RepoError> {
    let revision = super::plan_revision_repo::find(runner, scope, m.tenant_id, m.revision_id)
        .await?
        .ok_or(RepoError::Conflict {
            code: "REVISION_NOT_FOUND",
        })?;
    if revision.state != RevisionState::Draft.as_str() || revision.pending_unit_id.is_some() {
        return Err(RepoError::Conflict {
            code: "REVISION_NOT_DRAFT",
        });
    }
    if let Some(id) = m.price_book_entry_id {
        let entry = super::price_book_entry_repo::find(runner, scope, m.tenant_id, id)
            .await?
            .ok_or(RepoError::Conflict {
                code: "ENTRY_NOT_FOUND",
            })?;
        if entry.book_id != revision.book_id {
            return Err(RepoError::Conflict {
                code: "ITEM_BOOK_FOREIGN",
            });
        }
        if entry.sku_id != m.sku_id {
            return Err(RepoError::Conflict {
                code: "ITEM_ENTRY_SKU_MISMATCH",
            });
        }
    }
    let active = e::ActiveModel {
        id: Set(m.id),
        tenant_id: Set(m.tenant_id),
        revision_id: Set(m.revision_id),
        sku_id: Set(m.sku_id),
        price_book_entry_id: Set(m.price_book_entry_id),
        treatment: Set(m.treatment),
        included_qty: Set(m.included_qty),
        qty_min: Set(m.qty_min),
        reservation_id: Set(m.reservation_id),
        reference_state: Set(m.reference_state),
        version: Set(m.version),
        created_by: Set(m.created_by),
        created_at: Set(m.created_at),
        updated_at: Set(m.updated_at),
    };
    e::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| driver_failure("insert plan item scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map_err(|e| map_unique("insert plan item".into(), e))
}
/// Read by tenant and identity within the authorized scope.
/// # Errors
/// Returns typed database failures.
pub async fn find(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<Option<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(key(tenant, id))
        .one(runner)
        .await
        .map_err(|e| driver_failure("find plan item".into(), e))
}
/// A revision's items in stable identity order.
/// # Errors
/// Returns typed database failures.
pub async fn for_revision(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    revision_id: Uuid,
) -> Result<Vec<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::RevisionId.eq(revision_id)),
        )
        .order_by(e::Column::Id, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list plan items of a revision".into(), e))
}
/// The items of every revision among `revisions`, each revision's in identity order, in ONE
/// statement whatever their number (D-482). A revision without items has no rows.
///
/// # Errors
/// Returns typed database failures.
pub async fn for_revisions(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    revisions: &[Uuid],
) -> Result<Vec<e::Model>, RepoError> {
    if revisions.is_empty() {
        return Ok(Vec::new());
    }
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::RevisionId.is_in(revisions.iter().copied())),
        )
        .order_by(e::Column::RevisionId, Order::Asc)
        .order_by(e::Column::Id, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list plan items of revisions".into(), e))
}
/// One item's revision and SKU, a row of [`skus_of_revisions`].
#[derive(Debug, sea_orm::FromQueryResult)]
struct RevisionSkuRow {
    revision_id: Uuid,
    sku_id: Uuid,
}
/// The item SKUs of every revision among `revisions`, each revision's in ascending order, in ONE
/// statement whatever their number (D-460: the plans list's current revisions). The statement
/// runs for an empty list too (the query builder renders it `1 = 2`), so a list that reads it
/// makes the same statements for any number of rows; a revision without items has no key.
/// # Errors
/// Returns typed database failures.
pub async fn skus_of_revisions(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    revisions: &[Uuid],
) -> Result<std::collections::BTreeMap<Uuid, Vec<Uuid>>, RepoError> {
    use sea_orm::{QueryOrder, QuerySelect};
    let mut grouped: std::collections::BTreeMap<Uuid, Vec<Uuid>> =
        std::collections::BTreeMap::new();
    for row in e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::RevisionId.is_in(revisions.iter().copied())),
        )
        .project_all(runner, |q| {
            q.select_only()
                .column(e::Column::RevisionId)
                .column(e::Column::SkuId)
                .order_by(e::Column::RevisionId, Order::Asc)
                .order_by(e::Column::SkuId, Order::Asc)
                .into_model::<RevisionSkuRow>()
        })
        .await
        .map_err(|e| driver_failure("read the item SKUs of revisions".into(), e))?
    {
        grouped.entry(row.revision_id).or_default().push(row.sku_id);
    }
    Ok(grouped)
}
/// One item SKU, a row of [`skus_of_revision`].
#[derive(Debug, sea_orm::FromQueryResult)]
struct SkuRow {
    sku_id: Uuid,
}
/// The distinct SKUs the items of `revision` name, sorted, when the caller's plan read (`plans`)
/// reaches the revision: a picker's revision set (P-D-246), in ONE statement — the items joined to
/// their revision, the revision under `plans` as `GET /plan-revisions/{id}` reads it. A revision
/// the tenant does not hold, or one outside `plans`, is the empty set.
/// # Errors
/// Returns typed database failures.
pub async fn skus_of_revision(
    runner: &impl DBRunner,
    plans: &AccessScope,
    tenant: Uuid,
    revision: Uuid,
) -> Result<Vec<Uuid>, RepoError> {
    use sea_orm::{JoinType, QueryOrder, QuerySelect};
    let on_revision: sea_orm::RelationDef = e::Entity::belongs_to(plan_revision::Entity)
        .from(e::Column::RevisionId)
        .to(plan_revision::Column::Id)
        .into();
    Ok(e::Entity::find()
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .and_scope_for::<plan_revision::Entity>(plans)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::RevisionId.eq(revision))
                .add(
                    Expr::col((plan_revision::Entity, plan_revision::Column::TenantId)).eq(tenant),
                ),
        )
        .project_all(runner, |q| {
            q.select_only()
                .join(JoinType::InnerJoin, on_revision)
                .column(e::Column::SkuId)
                .distinct()
                .order_by(e::Column::SkuId, Order::Asc)
                .into_model::<SkuRow>()
        })
        .await
        .map_err(|e| driver_failure("list the SKUs of a revision's items".into(), e))?
        .into_iter()
        .map(|r| r.sku_id)
        .collect())
}
/// Whether any plan item names an entry (`ENTRY_IN_USE`, D-408).
/// # Errors
/// Returns typed database failures.
pub async fn names_entry(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    entry_id: Uuid,
) -> Result<bool, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::PriceBookEntryId.eq(entry_id)),
        )
        .one(runner)
        .await
        .map(|item| item.is_some())
        .map_err(|e| driver_failure("find plan item naming an entry".into(), e))
}
/// Every plan item, in any revision state, that names one of the entries: the plans a `prices`
/// unit's impact names (D-408).
/// # Errors
/// Returns typed database failures.
pub async fn naming_entries(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    entries: &[Uuid],
) -> Result<Vec<e::Model>, RepoError> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::PriceBookEntryId.is_in(entries.iter().copied())),
        )
        .order_by(e::Column::Id, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list plan items naming entries".into(), e))
}
/// Change an item's entry at the version the caller read, only while its revision is an unlocked
/// draft, and rewrite the row in D-467's shape whatever `m` carries (`d467_shape`); the SKU never
/// changes.
/// # Errors
/// `ENTRY_NOT_FOUND` for an entry outside the tenant; `STALE_REVISION` for a lost version or a
/// revision that is no longer an unlocked draft.
pub async fn update_draft(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<(), RepoError> {
    update_draft_as_given(runner, scope, d467_shape(m)).await
}
/// [`update_draft`] of `m` exactly as given, its treatment and quantities included: the legacy
/// row shape. No door writes through it; it seeds the legacy rows the suites read.
/// # Errors
/// As [`update_draft`].
pub async fn update_draft_as_given(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<(), RepoError> {
    entry_in_tenant(runner, scope, m.tenant_id, m.price_book_entry_id).await?;
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::Treatment, Expr::value(m.treatment))
        .col_expr(e::Column::IncludedQty, Expr::value(m.included_qty))
        .col_expr(e::Column::QtyMin, Expr::value(m.qty_min))
        .col_expr(
            e::Column::PriceBookEntryId,
            Expr::value(m.price_book_entry_id),
        )
        .col_expr(e::Column::UpdatedAt, Expr::value(m.updated_at))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(
            key(m.tenant_id, m.id)
                .add(e::Column::Version.eq(m.version))
                .add(e::Column::RevisionId.in_subquery(unlocked_drafts(m.tenant_id))),
        )
        .exec(runner)
        .await
        .map_err(|e| map_unique("update plan item".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}
/// Move the item's reference at the observed version, in any revision state: the reference
/// machine is the one writer allowed to touch an item of a published or superseded revision
/// (D-413).
/// # Errors
/// A concurrent change is `STALE_REVISION`; database failures keep their type.
#[expect(
    clippy::too_many_arguments,
    reason = "tenant identity, version and receipt are the conditional write operands"
)]
pub async fn set_reference(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    version: i64,
    state: ReferenceState,
    reservation_id: Option<Uuid>,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::ReferenceState, Expr::value(state.as_str()))
        .col_expr(e::Column::ReservationId, Expr::value(reservation_id))
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(key(tenant, id).add(e::Column::Version.eq(version)))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("update plan item reference".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}
/// Bounded identity-ordered scan for the trusted reconciliation worker: confirmed items, whose
/// receipts it checks, and lost items, which it re-reserves once their SKU admits a reservation
/// again. Items of every revision state are scanned: a revision's references outlive it (D-414).
/// # Errors
/// Returns typed scoped storage failures.
pub async fn reconcile_batch(
    runner: &impl DBRunner,
    scope: &AccessScope,
    cursor: Option<Uuid>,
    limit: u64,
) -> Result<Vec<e::Model>, RepoError> {
    let mut filter = Condition::all().add(e::Column::ReferenceState.is_in([
        ReferenceState::Confirmed.as_str(),
        ReferenceState::Lost.as_str(),
    ]));
    if let Some(cursor) = cursor {
        filter = filter.add(e::Column::Id.gt(cursor));
    }
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(filter)
        .order_by(e::Column::Id, Order::Asc)
        .limit(limit)
        .all(runner)
        .await
        .map_err(|e| driver_failure("confirmed plan item batch".into(), e))
}
/// Delete an item of an unlocked draft revision at its observed version; the caller writes its
/// delete op in the same transaction.
/// # Errors
/// A lost version or a revision that is no longer an unlocked draft is `STALE_REVISION`.
pub async fn delete_draft(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    version: i64,
) -> Result<(), RepoError> {
    use toolkit_db::secure::SecureDeleteExt;
    let result = e::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(
            key(tenant, id)
                .add(e::Column::Version.eq(version))
                .add(e::Column::RevisionId.in_subquery(unlocked_drafts(tenant))),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete draft plan item".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}
