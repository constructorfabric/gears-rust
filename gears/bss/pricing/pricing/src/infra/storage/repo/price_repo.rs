//! Scoped price persistence with conditional versions.
use super::{driver_failure, map_unique, matched};
use crate::domain::{
    price::{ChangeKind, PriceState},
    price_book_entry::ReferenceState,
};
use crate::infra::storage::{RepoError, entity::price as e};
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, QuerySelect, Set};
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use uuid::Uuid;
/// A `set` row: a price of its chain, not a `cancel` or an `end` that names one (D-520, D-521).
/// Only these rows are judged, counted, normalised or resolved as prices.
#[must_use]
pub fn is_price(m: &e::Model) -> bool {
    m.change_kind == ChangeKind::Set.as_str()
}
fn prices_only() -> sea_orm::sea_query::SimpleExpr {
    e::Column::ChangeKind.eq(ChangeKind::Set.as_str())
}
fn key(tenant: Uuid, id: Uuid) -> Condition {
    Condition::all()
        .add(e::Column::TenantId.eq(tenant))
        .add(e::Column::Id.eq(id))
}
/// Insert a tenant-scoped row in the caller's transaction.
/// # Errors
/// Returns unique conflicts, parent ownership refusals or typed database failures.
pub async fn insert(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<e::Model, RepoError> {
    let entry =
        super::price_book_entry_repo::find(runner, scope, m.tenant_id, m.price_book_entry_id)
            .await?
            .ok_or(RepoError::Conflict {
                code: "ENTRY_NOT_FOUND",
            })?;
    if entry.reference_state == ReferenceState::Lost.as_str() {
        return Err(RepoError::Conflict {
            code: "ENTRY_REFERENCE_LOST",
        });
    }
    // D-522: a released entry (its book archived, or not re-reserved since) takes no price.
    if entry.reference_state == ReferenceState::Released.as_str() {
        return Err(RepoError::Conflict {
            code: "ENTRY_REFERENCE_RELEASED",
        });
    }
    let active = e::ActiveModel {
        id: Set(m.id),
        tenant_id: Set(m.tenant_id),
        price_book_entry_id: Set(m.price_book_entry_id),
        version_no: Set(m.version_no),
        dim_value: Set(m.dim_value),
        price_json: Set(m.price_json),
        min_fee: Set(m.min_fee),
        eligibility: Set(m.eligibility),
        effective_from: Set(m.effective_from),
        effective_to: Set(m.effective_to),
        keep_for_bound: Set(m.keep_for_bound),
        closed_explicitly: Set(m.closed_explicitly),
        temporary_until: Set(m.temporary_until),
        paired_price_id: Set(m.paired_price_id),
        return_of_price_id: Set(m.return_of_price_id),
        state: Set(m.state),
        change_kind: Set(m.change_kind),
        target_price_id: Set(m.target_price_id),
        cancelled_by_unit_id: Set(m.cancelled_by_unit_id),
        pending_unit_id: Set(m.pending_unit_id),
        approved_by_unit_id: Set(m.approved_by_unit_id),
        note: Set(m.note),
        created_by: Set(m.created_by),
        approved_at: Set(m.approved_at),
        version: Set(m.version),
        created_at: Set(m.created_at),
        updated_at: Set(m.updated_at),
    };
    e::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| driver_failure("insert scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map_err(|e| map_unique("insert price".into(), e))
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
        .map_err(|e| driver_failure("find price".into(), e))
}
/// The tenant's prices among `ids`, by id, in ONE statement whatever their number; an id the
/// tenant does not hold has no row.
/// # Errors
/// Returns typed database failures.
pub async fn find_many(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    ids: &[Uuid],
) -> Result<Vec<e::Model>, RepoError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::Id.is_in(ids.iter().copied())),
        )
        .order_by(e::Column::Id, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list prices by id".into(), e))
}
/// Every price of the entries, by entry and then as [`for_entry`] orders one entry's (version
/// number), in ONE statement whatever the number of entries.
/// # Errors
/// Returns typed database failures.
pub async fn for_entries(
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
        .order_by(e::Column::PriceBookEntryId, Order::Asc)
        .order_by(e::Column::VersionNo, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list the prices of entries".into(), e))
}
/// [`for_entries`] grouped by entry, each entry's prices in [`for_entry`]'s order; an entry
/// without prices has no key.
#[must_use]
pub fn by_entry(prices: Vec<e::Model>) -> std::collections::BTreeMap<Uuid, Vec<e::Model>> {
    let mut groups: std::collections::BTreeMap<Uuid, Vec<e::Model>> =
        std::collections::BTreeMap::new();
    for p in prices {
        groups.entry(p.price_book_entry_id).or_default().push(p);
    }
    groups
}
/// List tenant rows in stable identity order.
/// # Errors
/// Returns typed database failures.
pub async fn list(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
) -> Result<Vec<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(e::Column::TenantId.eq(tenant)))
        .order_by(e::Column::Id, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list price".into(), e))
}
/// Change business columns only if the caller's version still owns the row.
/// # Errors
/// Zero matches is a typed version conflict; database failures preserve their type.
pub async fn update_draft(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<(), RepoError> {
    let predicate = key(m.tenant_id, m.id).add(e::Column::Version.eq(m.version));
    let predicate = predicate
        .add(e::Column::State.eq(PriceState::Draft.as_str()))
        .add(e::Column::PendingUnitId.is_null());
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::DimValue, Expr::value(m.dim_value))
        .col_expr(e::Column::PriceJson, Expr::value(m.price_json))
        .col_expr(e::Column::MinFee, Expr::value(m.min_fee))
        .col_expr(e::Column::Eligibility, Expr::value(m.eligibility))
        .col_expr(e::Column::EffectiveFrom, Expr::value(m.effective_from))
        .col_expr(e::Column::EffectiveTo, Expr::value(m.effective_to))
        .col_expr(
            e::Column::ClosedExplicitly,
            Expr::value(m.closed_explicitly),
        )
        .col_expr(e::Column::TemporaryUntil, Expr::value(m.temporary_until))
        .col_expr(e::Column::PairedPriceId, Expr::value(m.paired_price_id))
        .col_expr(
            e::Column::ReturnOfPriceId,
            Expr::value(m.return_of_price_id),
        )
        .col_expr(e::Column::Note, Expr::value(m.note))
        .col_expr(e::Column::UpdatedAt, Expr::value(m.updated_at))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(predicate)
        .exec(runner)
        .await
        .map_err(|e| map_unique("update price".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}
/// List rows of one scoped parent in stable order.
/// # Errors
/// Returns typed database failures.
pub async fn for_entry(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    parent: Uuid,
) -> Result<Vec<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::PriceBookEntryId.eq(parent)),
        )
        .order_by(e::Column::VersionNo, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list parent rows".into(), e))
}
/// How many prices of one entry are in one state, and of those how many have ended and how many
/// have not yet started on the day asked: a row of [`count_by_entry_and_state`].
#[derive(Debug, Clone, PartialEq, Eq, sea_orm::FromQueryResult)]
pub struct EntryStateCount {
    pub price_book_entry_id: Uuid,
    pub state: String,
    pub count: i64,
    /// Of `count`, the prices whose window ends on or before the day: an approved one is
    /// `superseded` (D-440).
    pub ended: i64,
    /// Of `count`, the prices not ended that start after the day: an approved one is `scheduled`;
    /// the rest of the approved are `active` (D-440).
    pub future: i64,
}
/// `SUM(CASE WHEN … THEN 1 ELSE 0 END)` of the prices whose window has ended on `today`, and of
/// those not ended that start after it: `domain::price::window_display`'s order, superseded first.
fn dated_sums(today: time::Date) -> (Expr, Expr) {
    use sea_orm::sea_query::Func;
    let to = || Expr::col((e::Entity, e::Column::EffectiveTo));
    let ended = to().is_not_null().and(to().lte(today));
    let future = to()
        .is_null()
        .or(to().gt(today))
        .and(Expr::col((e::Entity, e::Column::EffectiveFrom)).gt(today));
    let sum = |condition: Expr| {
        Expr::from(Func::sum(
            Expr::case(condition, Expr::cust("1")).finally(Expr::cust("0")),
        ))
    };
    (sum(ended), sum(future))
}
/// The prices of the entries, counted by entry and state in ONE grouped statement, whatever the
/// number of entries (D-428), with the ended and the future ones of each group on `today` (D-440).
/// An entry without prices has no row.
/// # Errors
/// Returns typed database failures.
pub async fn count_by_entry_and_state(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    entries: &[Uuid],
    today: time::Date,
) -> Result<Vec<EntryStateCount>, RepoError> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let (ended, future) = dated_sums(today);
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::PriceBookEntryId.is_in(entries.iter().copied()))
                .add(prices_only()),
        )
        .project_all(runner, |q| {
            q.select_only()
                .column(e::Column::PriceBookEntryId)
                .column(e::Column::State)
                .column_as(Expr::col(e::Column::Id).count(), "count")
                .column_as(ended, "ended")
                .column_as(future, "future")
                .group_by(e::Column::PriceBookEntryId)
                .group_by(e::Column::State)
                .into_model::<EntryStateCount>()
        })
        .await
        .map_err(|e| driver_failure("count prices by entry and state".into(), e))
}
/// How many prices of one book's entries are in one state, the ended and future ones among them,
/// and their latest `updated_at`: a row of [`count_by_book_and_state`].
#[derive(Debug, Clone, PartialEq, Eq, sea_orm::FromQueryResult)]
pub struct BookStateCount {
    pub book_id: Uuid,
    pub state: String,
    pub count: i64,
    pub ended: i64,
    pub future: i64,
    /// [`super::latest`] of the group's `updated_at`.
    pub latest: Option<String>,
}
/// The prices of the books' entries, counted by book and state in ONE grouped statement whatever
/// the number of books, entries and prices (D-441): each price joined to its one entry, so no row
/// is counted twice. A book without prices has no row.
/// # Errors
/// Returns typed database failures.
pub async fn count_by_book_and_state(
    runner: &impl DBRunner,
    tenant: Uuid,
    backend: sea_orm::DbBackend,
    books: &[Uuid],
    today: time::Date,
) -> Result<Vec<BookStateCount>, RepoError> {
    use crate::infra::storage::entity::price_book_entry as entry;
    use sea_orm::JoinType;
    if books.is_empty() {
        return Ok(Vec::new());
    }
    let on_entry: sea_orm::RelationDef = e::Entity::belongs_to(entry::Entity)
        .from(e::Column::PriceBookEntryId)
        .to(entry::Column::Id)
        .into();
    let (ended, future) = dated_sums(today);
    let latest = super::latest(backend, Expr::col((e::Entity, e::Column::UpdatedAt)));
    e::Entity::find()
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(prices_only())
                .add(Expr::col((entry::Entity, entry::Column::TenantId)).eq(tenant))
                .add(
                    Expr::col((entry::Entity, entry::Column::BookId)).is_in(books.iter().copied()),
                ),
        )
        .project_all(runner, |q| {
            q.select_only()
                .join(JoinType::InnerJoin, on_entry)
                .column_as(Expr::col((entry::Entity, entry::Column::BookId)), "book_id")
                .column(e::Column::State)
                .column_as(Expr::col((e::Entity, e::Column::Id)).count(), "count")
                .column_as(ended, "ended")
                .column_as(future, "future")
                .column_as(latest, "latest")
                .group_by(Expr::col((entry::Entity, entry::Column::BookId)))
                .group_by(e::Column::State)
                .into_model::<BookStateCount>()
        })
        .await
        .map_err(|e| driver_failure("count prices by book and state".into(), e))
}
/// How many prices of any state carry one value of one dimension key: a row of
/// [`count_by_key_and_value`].
#[derive(Debug, Clone, PartialEq, Eq, sea_orm::FromQueryResult)]
pub struct KeyValueCount {
    pub dimension_key: String,
    pub dim_value: String,
    pub count: i64,
}
/// The tenant's prices of EVERY state (a rejected or pending price still carries its value)
/// counted by their entry's dimension key and their own value, in ONE grouped statement whatever
/// the number of entries and prices (D-436). A value no price carries has no row.
/// # Errors
/// Returns typed database failures.
pub async fn count_by_key_and_value(
    runner: &impl DBRunner,
    tenant: Uuid,
) -> Result<Vec<KeyValueCount>, RepoError> {
    use crate::infra::storage::entity::price_book_entry as entry;
    use sea_orm::JoinType;
    let on_entry: sea_orm::RelationDef = e::Entity::belongs_to(entry::Entity)
        .from(e::Column::PriceBookEntryId)
        .to(entry::Column::Id)
        .into();
    e::Entity::find()
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(prices_only())
                .add(e::Column::DimValue.is_not_null())
                .add(Expr::col((entry::Entity, entry::Column::TenantId)).eq(tenant))
                .add(Expr::col((entry::Entity, entry::Column::DimensionKey)).is_not_null()),
        )
        .project_all(runner, |q| {
            q.select_only()
                .join(JoinType::InnerJoin, on_entry)
                .column(entry::Column::DimensionKey)
                .column(e::Column::DimValue)
                .column_as(Expr::col((e::Entity, e::Column::Id)).count(), "count")
                .group_by(entry::Column::DimensionKey)
                .group_by(e::Column::DimValue)
                .into_model::<KeyValueCount>()
        })
        .await
        .map_err(|e| driver_failure("count prices by dimension key and value".into(), e))
}
/// The approved, pending and draft prices of the DEFAULT chain (no dimension value) of the
/// entries — never a rejected one — in ONE statement whatever their number (D-434, D-472): what the
/// price in force and the next price of each are chosen from.
/// # Errors
/// Returns typed database failures.
pub async fn default_chain(
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
                .add(e::Column::PriceBookEntryId.is_in(entries.iter().copied()))
                .add(e::Column::State.is_in([
                    PriceState::Approved.as_str(),
                    PriceState::Pending.as_str(),
                    PriceState::Draft.as_str(),
                ]))
                .add(prices_only())
                .add(e::Column::DimValue.is_null()),
        )
        .order_by(e::Column::Id, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list default-chain prices".into(), e))
}
/// Remove a draft only at its current version and outside an approval unit.
/// # Errors
/// Refuses stale versions, non-drafts and pending ownership.
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
                .add(e::Column::State.eq(PriceState::Draft.as_str()))
                .add(e::Column::PendingUnitId.is_null()),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete draft".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}
/// Decode a stored price into the pure model, in its ENTRY's model (D-427: a price has no model
/// of its own; the caller passes `price_book_entry_repo::model_of` of the price's entry). Unknown
/// vocabulary, or money whose shape is not the entry's model, is a corrupt row.
/// # Errors
/// Returns `CorruptRow` for a stored enum or price shape the model does not know.
pub fn to_domain(
    m: &e::Model,
    model: crate::domain::price_book_entry::Model,
) -> Result<crate::domain::price::Price, RepoError> {
    use crate::domain::{money, price};
    let corrupt = |what: &str| RepoError::CorruptRow(format!("price {} {what}", m.id));
    Ok(price::Price {
        id: m.id,
        price_book_entry_id: m.price_book_entry_id,
        version_no: m.version_no,
        dim_value: m.dim_value.clone(),
        model,
        price: Some(money::decode(model, m.price_json.clone()).map_err(|_| corrupt("price_json"))?),
        min_fee: m
            .min_fee
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(|_| corrupt("min_fee"))?,
        eligibility: m.eligibility.parse().map_err(|_| corrupt("eligibility"))?,
        effective_from: m.effective_from,
        effective_to: m.effective_to,
        temporary_until: m.temporary_until,
        paired_price_id: m.paired_price_id,
        return_of_price_id: m.return_of_price_id,
        closed_explicitly: m.closed_explicitly,
        state: m.state.parse().map_err(|_| corrupt("state"))?,
    })
}
/// Point a freshly inserted draft at its pair partner, without a version step.
/// The partner must exist first: the pair reference is a foreign key.
/// # Errors
/// Refuses anything but an unlinked, unlocked draft; preserves database failures.
pub async fn link_pair(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    partner: Uuid,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::PairedPriceId, Expr::value(Some(partner)))
        .filter(
            key(tenant, id)
                .add(e::Column::State.eq(PriceState::Draft.as_str()))
                .add(e::Column::PendingUnitId.is_null())
                .add(e::Column::PairedPriceId.is_null()),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("link pair".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}
/// Delete unlocked drafts, each at its observed version, in ONE statement so a
/// pair's mutual references never dangle between two deletes.
/// # Errors
/// Refuses when any price moved or is no longer an unlocked draft.
pub async fn delete_drafts(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    prices: &[(Uuid, i64)],
) -> Result<(), RepoError> {
    use toolkit_db::secure::SecureDeleteExt;
    if prices.is_empty() {
        return Ok(());
    }
    let mut any = Condition::any();
    for (id, version) in prices {
        any = any.add(
            Condition::all()
                .add(e::Column::Id.eq(*id))
                .add(e::Column::Version.eq(*version)),
        );
    }
    let result = e::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(any)
                .add(e::Column::State.eq(PriceState::Draft.as_str()))
                .add(e::Column::PendingUnitId.is_null()),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete drafts".into(), e))?;
    if usize::try_from(result.rows_affected).ok() == Some(prices.len()) {
        Ok(())
    } else {
        Err(RepoError::Conflict {
            code: "STALE_REVISION",
        })
    }
}
/// Delete every price of an entry being deleted, in one statement: only drafts and rejected
/// prices, none owned by a pending unit, each at its observed version. A rejected price's review
/// history stays in its approval unit's snapshot.
/// # Errors
/// A price that changed or is not deletable is a `STALE_REVISION`; database failures keep
/// their type.
pub async fn delete_unapproved(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    prices: &[(Uuid, i64)],
) -> Result<(), RepoError> {
    use toolkit_db::secure::SecureDeleteExt;
    if prices.is_empty() {
        return Ok(());
    }
    let mut any = Condition::any();
    for (id, version) in prices {
        any = any.add(
            Condition::all()
                .add(e::Column::Id.eq(*id))
                .add(e::Column::Version.eq(*version)),
        );
    }
    let result = e::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(any)
                .add(
                    e::Column::State
                        .is_in([PriceState::Draft.as_str(), PriceState::Rejected.as_str()]),
                )
                .add(e::Column::PendingUnitId.is_null()),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete unapproved prices".into(), e))?;
    if usize::try_from(result.rows_affected).ok() == Some(prices.len()) {
        Ok(())
    } else {
        Err(RepoError::Conflict {
            code: "STALE_REVISION",
        })
    }
}
/// Run price apply work under serializable isolation, retrying driver contention.
/// The approval subject and doors use this boundary when they arrive in Task 2c.7.
/// # Errors
/// Returns the final business refusal or original database failure after bounded retries.
pub async fn transaction<T: Send + 'static>(
    db: &toolkit_db::Db,
    work: impl for<'a> FnMut(
        &'a toolkit_db::DbTx<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, RepoError>> + Send + 'a>,
    > + Send,
) -> Result<T, RepoError> {
    db.transaction_with_retry(
        toolkit_db::secure::TxConfig::serializable(),
        |error| match error {
            RepoError::Driver { source, .. } => Some(source),
            _ => None,
        },
        work,
    )
    .await
}
/// Acquire pending ownership only on an unlocked draft at the observed version.
/// # Errors
/// Returns missing-unit or typed database failures. A lost race returns false.
pub async fn try_lock(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    unit: Uuid,
    version: i64,
) -> Result<bool, RepoError> {
    let parent = super::approval_repo::find_unit(runner, scope, tenant, unit)
        .await
        .map_err(|error| {
            error.db_err().map_or_else(
                || RepoError::Db(error.to_string()),
                |source| RepoError::Driver {
                    context: "price unit".into(),
                    source: source.clone(),
                },
            )
        })?;
    if parent.is_none() {
        return Err(RepoError::Conflict {
            code: "UNIT_NOT_FOUND",
        });
    }
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::PendingUnitId, Expr::value(Some(unit)))
        .col_expr(e::Column::State, Expr::value(PriceState::Pending.as_str()))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(
            key(tenant, id)
                .add(e::Column::Version.eq(version))
                .add(e::Column::State.eq(PriceState::Draft.as_str()))
                .add(e::Column::PendingUnitId.is_null()),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("lock price conditionally".into(), e))?;
    Ok(result.rows_affected == 1)
}
/// What closing a unit leaves on each of its prices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unlock {
    /// Apply already approved the price; the lock turns into `approved_by_unit_id`.
    Approved,
    /// Withdrawn: the price is an editable draft again.
    Draft,
    /// Rejected: the price keeps its review history and stays rejected.
    Rejected,
}
/// Release only the owning unit's price; approved prices retain the unit identity.
/// # Errors
/// Returns a conditional conflict or typed database failure.
pub async fn unlock(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    unit: Uuid,
    outcome: Unlock,
) -> Result<(), RepoError> {
    let (state, from) = match outcome {
        Unlock::Approved => (PriceState::Approved, PriceState::Approved),
        Unlock::Draft => (PriceState::Draft, PriceState::Pending),
        Unlock::Rejected => (PriceState::Rejected, PriceState::Pending),
    };
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::PendingUnitId, Expr::value(None::<Uuid>))
        .col_expr(
            e::Column::ApprovedByUnitId,
            Expr::value((outcome == Unlock::Approved).then_some(unit)),
        )
        .col_expr(e::Column::State, Expr::value(state.as_str()))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(
            key(tenant, id)
                .add(e::Column::PendingUnitId.eq(unit))
                .add(e::Column::State.eq(from.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| map_unique("unlock price".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}
/// The window an applied price takes, after the unit's shift and the chain's normalisation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Approval {
    pub effective_from: time::Date,
    pub effective_to: Option<time::Date>,
    pub temporary_until: Option<time::Date>,
    pub keep_for_bound: bool,
}
/// Approve a price the unit holds; the approved-start index arbitrates a racing chain.
/// # Errors
/// `WINDOW_OVERLAP` when the chain already has an approved price on that start,
/// `PRICE_NOT_PENDING` when the unit does not hold the price.
pub async fn approve(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    unit: Uuid,
    window: Approval,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::State, Expr::value(PriceState::Approved.as_str()))
        .col_expr(e::Column::EffectiveFrom, Expr::value(window.effective_from))
        .col_expr(e::Column::EffectiveTo, Expr::value(window.effective_to))
        .col_expr(
            e::Column::TemporaryUntil,
            Expr::value(window.temporary_until),
        )
        .col_expr(e::Column::KeepForBound, Expr::value(window.keep_for_bound))
        .col_expr(e::Column::ApprovedAt, Expr::value(Some(now)))
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(
            key(tenant, id)
                .add(e::Column::PendingUnitId.eq(unit))
                .add(e::Column::State.eq(PriceState::Pending.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| map_unique("approve price".into(), e))?;
    matched(result.rows_affected, "PRICE_NOT_PENDING")
}
/// Re-close an approved price after its chain changed, at the version the caller read.
/// # Errors
/// A concurrent change is `STALE_REVISION`; database failures keep their type.
#[expect(
    clippy::too_many_arguments,
    reason = "tenant identity, version and the two recomputed columns are the write's operands"
)]
pub async fn set_window(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    version: i64,
    effective_to: Option<time::Date>,
    keep_for_bound: bool,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::EffectiveTo, Expr::value(effective_to))
        .col_expr(e::Column::KeepForBound, Expr::value(keep_for_bound))
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(
            key(tenant, id)
                .add(e::Column::Version.eq(version))
                .add(e::Column::State.eq(PriceState::Approved.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| map_unique("re-close price".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}
/// Cancel an approved price at the version the caller read: it becomes `cancelled`, records
/// the unit that cancelled it and leaves every chain (D-520).
/// # Errors
/// A price that is no longer approved at `version` is `STALE_REVISION`; database failures keep
/// their type.
pub async fn cancel(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    version: i64,
    unit: Uuid,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            e::Column::State,
            Expr::value(PriceState::Cancelled.as_str()),
        )
        .col_expr(e::Column::CancelledByUnitId, Expr::value(Some(unit)))
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(
            key(tenant, id)
                .add(e::Column::Version.eq(version))
                .add(e::Column::State.eq(PriceState::Approved.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| map_unique("cancel price".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}
/// End an approved price explicitly at `effective_to`, at the version the caller read (D-521):
/// the end survives every later normalisation unless a successor starts inside it (D-390).
/// # Errors
/// A price that is no longer approved at `version` is `STALE_REVISION`; database failures keep
/// their type.
#[expect(
    clippy::too_many_arguments,
    reason = "tenant identity, version and the two recomputed columns are the write's operands"
)]
pub async fn close_explicitly(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    version: i64,
    effective_to: time::Date,
    keep_for_bound: bool,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::EffectiveTo, Expr::value(Some(effective_to)))
        .col_expr(e::Column::ClosedExplicitly, Expr::value(true))
        .col_expr(e::Column::KeepForBound, Expr::value(keep_for_bound))
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(
            key(tenant, id)
                .add(e::Column::Version.eq(version))
                .add(e::Column::State.eq(PriceState::Approved.as_str())),
        )
        .exec(runner)
        .await
        .map_err(|e| map_unique("end price".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}
