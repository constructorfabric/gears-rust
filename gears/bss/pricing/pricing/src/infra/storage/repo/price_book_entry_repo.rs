//! Scoped price book entry persistence with conditional versions.
use super::{driver_failure, map_unique, matched};
use crate::infra::storage::{RepoError, entity::price_book_entry as e};
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, Set};
use toolkit_db::odata::sea_orm_filter::{
    FieldToColumn, LimitCfg, ODataFieldMapping, PaginateOdataTryError, paginate_odata_try,
};
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_odata::filter::{FieldKind, FilterField, FilterOp, ODataValue};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, Page, SortDir};
use uuid::Uuid;
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
    if super::book_repo::find(runner, scope, m.tenant_id, m.book_id)
        .await?
        .is_none()
    {
        return Err(RepoError::Conflict {
            code: "BOOK_NOT_FOUND",
        });
    }
    if let Some(dimension) = &m.dimension_key
        && !super::dimension_repo::declare_for_entry(runner, scope, m.tenant_id, dimension).await?
    {
        return Err(RepoError::Conflict {
            code: "DIM_NOT_DECLARED",
        });
    }
    let active = e::ActiveModel {
        id: Set(m.id),
        tenant_id: Set(m.tenant_id),
        book_id: Set(m.book_id),
        sku_id: Set(m.sku_id),
        charge_kind: Set(m.charge_kind),
        period: Set(m.period),
        model: Set(m.model),
        usage_policy_id: Set(m.usage_policy_id),
        usage_policy_version: Set(m.usage_policy_version),
        usage_policy_digest: Set(m.usage_policy_digest),
        usage_sku_version: Set(m.usage_sku_version),
        dimension_key: Set(m.dimension_key),
        invoice_line_override: Set(m.invoice_line_override),
        reservation_id: Set(m.reservation_id),
        reference_state: Set(m.reference_state),
        version: Set(m.version),
        created_at: Set(m.created_at),
        updated_at: Set(m.updated_at),
    };
    e::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| driver_failure("insert price book entry scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map_err(|e| map_unique("insert price book entry".into(), e))
}
/// The entry's model in the pure model (D-427): every price of the entry is decoded and judged
/// with it. Unknown vocabulary is a corrupt row.
/// # Errors
/// `CorruptRow` for a stored model the domain does not know.
pub fn model_of(m: &e::Model) -> Result<crate::domain::price_book_entry::Model, RepoError> {
    m.model
        .parse()
        .map_err(|_| RepoError::CorruptRow(format!("entry {} model", m.id)))
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
        .map_err(|e| driver_failure("find price book entry".into(), e))
}
/// The tenant's entries among `ids`, by id, in ONE statement whatever their number; an id the
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
        .map_err(|e| driver_failure("list price book entries by id".into(), e))
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
        .map_err(|e| driver_failure("list price book entries".into(), e))
}
/// Change business columns only if the caller's version still owns the row.
/// # Errors
/// Zero matches is a typed version conflict; database failures preserve their type.
pub async fn update(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<(), RepoError> {
    let predicate = key(m.tenant_id, m.id).add(e::Column::Version.eq(m.version));
    if let Some(dimension) = &m.dimension_key
        && !super::dimension_repo::declare_for_entry(runner, scope, m.tenant_id, dimension).await?
    {
        return Err(RepoError::Conflict {
            code: "DIM_NOT_DECLARED",
        });
    }
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::DimensionKey, Expr::value(m.dimension_key))
        .col_expr(
            e::Column::InvoiceLineOverride,
            Expr::value(m.invoice_line_override),
        )
        .col_expr(e::Column::UpdatedAt, Expr::value(m.updated_at))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(predicate)
        .exec(runner)
        .await
        .map_err(|e| map_unique("update price book entry".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}
/// List rows of one scoped parent in stable order.
/// # Errors
/// Returns typed database failures.
pub async fn for_book(
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
                .add(e::Column::BookId.eq(parent)),
        )
        .order_by(e::Column::Id, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list price book entries of a book".into(), e))
}
// ------------------------------------------------------------------ a book's entries pager (D-483)

/// The page size when the caller names none, and the most a page holds (`limit` is clamped): a
/// book of 500 entries or fewer stays on one page (D-483).
pub const ENTRY_PAGE: LimitCfg = LimitCfg {
    default: 500,
    max: 500,
};

/// Every field of a book's entries pager: the filter fields the door publishes (`sku_id`,
/// `charge_kind`, `model`, `reference_state`) and the order's keys (`sku_id`, `charge_kind`,
/// `model` and the tie-break `id`), every one non-null, as the cursor codec needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryListField {
    Id,
    SkuId,
    ChargeKind,
    Model,
    ReferenceState,
}
impl FilterField for EntryListField {
    const FIELDS: &'static [Self] = &[
        Self::Id,
        Self::SkuId,
        Self::ChargeKind,
        Self::Model,
        Self::ReferenceState,
    ];
    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::SkuId => "sku_id",
            Self::ChargeKind => "charge_kind",
            Self::Model => "model",
            Self::ReferenceState => "reference_state",
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::Id | Self::SkuId => FieldKind::Uuid,
            Self::ChargeKind | Self::Model | Self::ReferenceState => FieldKind::String,
        }
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}
/// The order of a book's entries (D-483): `(sku_id, charge_kind, model)` ascending, then `id`, the
/// pager's tie-break. `period` is not a key: it is null for usage and one-time entries, and the
/// cursor codec cannot carry a null.
pub const ENTRY_ORDER: [EntryListField; 3] = [
    EntryListField::SkuId,
    EntryListField::ChargeKind,
    EntryListField::Model,
];
/// How the pager reads the entry row for each field.
pub struct EntryListMapping;
impl FieldToColumn<EntryListField> for EntryListMapping {
    type Column = e::Column;
    fn map_field(field: EntryListField) -> e::Column {
        match field {
            EntryListField::Id => e::Column::Id,
            EntryListField::SkuId => e::Column::SkuId,
            EntryListField::ChargeKind => e::Column::ChargeKind,
            EntryListField::Model => e::Column::Model,
            EntryListField::ReferenceState => e::Column::ReferenceState,
        }
    }
    /// `reference_state` filters only.
    fn is_orderable(field: EntryListField) -> bool {
        !matches!(field, EntryListField::ReferenceState)
    }
    /// `charge_kind`, `model` and `reference_state` compare (`eq`, `ne`, `in`) with one of their
    /// closed values only, so a filter never names a token no entry can hold. The text functions
    /// take any text there, as on any text field.
    fn map_value(
        field: EntryListField,
        op: FilterOp,
        value: &ODataValue,
    ) -> Result<ODataValue, String> {
        use crate::domain::price_book_entry::{ChargeKind, Model, ReferenceState};
        use std::str::FromStr;
        let closed = matches!(op, FilterOp::Eq | FilterOp::Ne | FilterOp::In);
        if let (true, ODataValue::String(token)) = (closed, value) {
            let known = match field {
                EntryListField::ChargeKind => Some(ChargeKind::from_str(token).is_ok()),
                EntryListField::Model => Some(Model::from_str(token).is_ok()),
                EntryListField::ReferenceState => Some(ReferenceState::from_str(token).is_ok()),
                EntryListField::Id | EntryListField::SkuId => None,
            };
            if known == Some(false) {
                return Err(format!(
                    "`{token}` is not a {} an entry holds",
                    field.name()
                ));
            }
        }
        Ok(value.clone())
    }
}
impl ODataFieldMapping<EntryListField> for EntryListMapping {
    type Entity = e::Entity;
    fn extract_cursor_value(model: &e::Model, field: EntryListField) -> sea_orm::Value {
        match field {
            EntryListField::Id => sea_orm::Value::Uuid(Some(model.id)),
            EntryListField::SkuId => sea_orm::Value::Uuid(Some(model.sku_id)),
            EntryListField::ChargeKind => sea_orm::Value::String(Some(model.charge_kind.clone())),
            EntryListField::Model => sea_orm::Value::String(Some(model.model.clone())),
            EntryListField::ReferenceState => {
                sea_orm::Value::String(Some(model.reference_state.clone()))
            }
        }
    }
}
/// A page read refused or failed.
#[derive(Debug)]
pub enum EntryListError {
    /// The query itself: a filter value, a cursor (400).
    Query(toolkit_odata::Error),
    /// Storage; a driver failure keeps its message for the retry classifier.
    Repo(RepoError),
}
/// One page of `book`'s entries under `scope` (D-483): the query's `$filter` and cursor, in
/// [`ENTRY_ORDER`] tie-broken by `id`; `limit` defaults to 500 and is clamped at 500. The door
/// takes no `$orderby`: a query without a cursor is ordered here, and a cursor carries the order.
/// ONE statement.
/// # Errors
/// [`EntryListError::Query`] for a value or a cursor the pager refuses; [`EntryListError::Repo`]
/// for storage.
pub async fn page_of_book(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    book: Uuid,
    query: &ODataQuery,
) -> Result<Page<e::Model>, EntryListError> {
    let mut query = query.clone();
    if query.cursor.is_none() {
        query.order = ODataOrderBy(
            ENTRY_ORDER
                .iter()
                .map(|field| OrderKey {
                    field: field.name().to_owned(),
                    dir: SortDir::Asc,
                })
                .collect(),
        );
    }
    let select = e::Entity::find().secure().scope_with(scope).filter(
        Condition::all()
            .add(e::Column::TenantId.eq(tenant))
            .add(e::Column::BookId.eq(book)),
    );
    paginate_odata_try::<EntryListField, EntryListMapping, e::Entity, e::Model, _, RepoError, _>(
        select,
        runner,
        &query,
        (EntryListField::Id.name(), SortDir::Asc),
        ENTRY_PAGE,
        Ok,
    )
    .await
    .map_err(|e| match e {
        // The pager renders the driver's error as text; kept as a driver failure so the door's
        // retry still sees a serialization failure or a busy database by its message.
        PaginateOdataTryError::OData(toolkit_odata::Error::Db(message)) => {
            EntryListError::Repo(RepoError::Driver {
                context: "list a page of a book's price book entries".into(),
                source: sea_orm::DbErr::Custom(message),
            })
        }
        PaginateOdataTryError::OData(other) => EntryListError::Query(other),
        PaginateOdataTryError::MapError(e) => EntryListError::Repo(e),
    })
}
/// The tenant's entries of the SKUs, in every book and every reference state, in ONE statement
/// (D-428).
/// # Errors
/// Returns typed database failures.
pub async fn for_skus(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    skus: &[Uuid],
) -> Result<Vec<e::Model>, RepoError> {
    if skus.is_empty() {
        return Ok(Vec::new());
    }
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::SkuId.is_in(skus.iter().copied())),
        )
        .order_by(e::Column::Id, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list price book entries of SKUs".into(), e))
}
/// One dimension key an entry names.
#[derive(Debug, sea_orm::FromQueryResult)]
struct KeyRow {
    dimension_key: String,
}
/// The distinct dimension keys the tenant's entries name, in any reference state, sorted, in ONE
/// statement whatever the number of entries (D-436): a key one names is not removed
/// (`DIMENSION_KEY_IN_USE`).
/// # Errors
/// Returns typed database failures.
pub async fn named_keys(runner: &impl DBRunner, tenant: Uuid) -> Result<Vec<String>, RepoError> {
    use sea_orm::{QueryOrder, QuerySelect};
    Ok(e::Entity::find()
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::DimensionKey.is_not_null()),
        )
        .project_all(runner, |q| {
            q.select_only()
                .column(e::Column::DimensionKey)
                .distinct()
                .order_by(e::Column::DimensionKey, Order::Asc)
                .into_model::<KeyRow>()
        })
        .await
        .map_err(|e| driver_failure("list the dimension keys price book entries name".into(), e))?
        .into_iter()
        .map(|r| r.dimension_key)
        .collect())
}
/// How many entries one book holds, over how many distinct SKUs, and their latest `updated_at`: a
/// row of [`count_by_book`].
#[derive(Debug, Clone, PartialEq, Eq, sea_orm::FromQueryResult)]
pub struct BookEntryCount {
    pub book_id: Uuid,
    pub entries: i64,
    pub skus: i64,
    /// [`super::latest`] of the book's entries' `updated_at`.
    pub latest: Option<String>,
}
/// The entries of the tenant's `books`, in every reference state, counted by book in ONE grouped
/// statement whatever the number of books and entries (D-441). A book without entries has no row.
/// # Errors
/// Returns typed database failures.
pub async fn count_by_book(
    runner: &impl DBRunner,
    tenant: Uuid,
    backend: sea_orm::DbBackend,
    books: &[Uuid],
) -> Result<Vec<BookEntryCount>, RepoError> {
    use sea_orm::QuerySelect;
    use sea_orm::sea_query::Func;
    if books.is_empty() {
        return Ok(Vec::new());
    }
    e::Entity::find()
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::BookId.is_in(books.iter().copied())),
        )
        .project_all(runner, |q| {
            q.select_only()
                .column(e::Column::BookId)
                .column_as(Expr::col((e::Entity, e::Column::Id)).count(), "entries")
                .column_as(
                    Expr::from(Func::count_distinct(Expr::col((
                        e::Entity,
                        e::Column::SkuId,
                    )))),
                    "skus",
                )
                .column_as(
                    super::latest(backend, Expr::col((e::Entity, e::Column::UpdatedAt))),
                    "latest",
                )
                .group_by(e::Column::BookId)
                .into_model::<BookEntryCount>()
        })
        .await
        .map_err(|e| driver_failure("count price book entries by book".into(), e))
}
/// The tenant's books with an entry of `sku`, in any reference state (D-442): `id IN (SELECT
/// book_id …)`, a condition on the book list's statement, never a statement of its own.
#[must_use]
pub fn books_pricing(tenant: Uuid, sku: Uuid) -> sea_orm::sea_query::SelectStatement {
    sea_orm::sea_query::Query::select()
        .column((e::Entity, e::Column::BookId))
        .from(e::Entity)
        .and_where(Expr::col((e::Entity, e::Column::TenantId)).eq(tenant))
        .and_where(Expr::col((e::Entity, e::Column::SkuId)).eq(sku))
        .to_owned()
}
/// One SKU id of a set read.
#[derive(Debug, sea_orm::FromQueryResult)]
struct SkuIdRow {
    sku_id: Uuid,
}
/// The distinct SKU ids of the tenant's entries under `scope`, narrowed by `condition`, sorted,
/// in ONE statement.
async fn distinct_skus(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    condition: Condition,
    context: &str,
) -> Result<Vec<Uuid>, RepoError> {
    use sea_orm::{QueryOrder, QuerySelect};
    Ok(e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(condition),
        )
        .project_all(runner, |q| {
            q.select_only()
                .column(e::Column::SkuId)
                .distinct()
                .order_by(e::Column::SkuId, Order::Asc)
                .into_model::<SkuIdRow>()
        })
        .await
        .map_err(|e| driver_failure(context.into(), e))?
        .into_iter()
        .map(|r| r.sku_id)
        .collect())
}
/// The SKUs with an entry in any book of the tenant, in any reference state, under `scope`: the
/// SKUs whose usage counts an entry (D-428), in ONE statement (P-D-212).
/// # Errors
/// Returns typed database failures.
pub async fn priced_skus(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
) -> Result<Vec<Uuid>, RepoError> {
    distinct_skus(
        runner,
        scope,
        tenant,
        Condition::all(),
        "list the SKUs of price book entries",
    )
    .await
}
/// The SKUs with an entry in `book`, in any reference state, under `scope`: a picker's book set
/// (P-D-246), in ONE statement. A book the tenant does not hold has no entry: the empty set.
/// # Errors
/// Returns typed database failures.
pub async fn skus_in_book(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    book: Uuid,
) -> Result<Vec<Uuid>, RepoError> {
    distinct_skus(
        runner,
        scope,
        tenant,
        Condition::all().add(e::Column::BookId.eq(book)),
        "list the SKUs of a book's price book entries",
    )
    .await
}
/// The SKUs whose entries (under `scope`) a plan item of a draft, pending, scheduled or published
/// revision names: the SKUs whose usage counts a plan (D-428), in ONE statement (P-D-212). The
/// items and revisions are read tenant-scoped, as the usage count reads them.
/// # Errors
/// Returns typed database failures.
pub async fn in_plan_skus(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
) -> Result<Vec<Uuid>, RepoError> {
    use crate::domain::plan::RevisionState;
    use crate::infra::storage::entity::{plan_item as item, plan_revision as revision};
    let live_item = sea_orm::sea_query::Query::select()
        .expr(Expr::val(1))
        .from(item::Entity)
        .inner_join(
            revision::Entity,
            Expr::col((revision::Entity, revision::Column::Id))
                .equals((item::Entity, item::Column::RevisionId)),
        )
        .and_where(Expr::col((item::Entity, item::Column::TenantId)).eq(tenant))
        .and_where(
            Expr::col((item::Entity, item::Column::PriceBookEntryId))
                .equals((e::Entity, e::Column::Id)),
        )
        .and_where(Expr::col((revision::Entity, revision::Column::TenantId)).eq(tenant))
        .and_where(
            Expr::col((revision::Entity, revision::Column::State))
                .ne(RevisionState::Superseded.as_str()),
        )
        .to_owned();
    distinct_skus(
        runner,
        scope,
        tenant,
        Condition::all().add(Expr::exists(live_item)),
        "list the SKUs of price book entries in a plan",
    )
    .await
}
/// Change the reference receipt/state at the observed version.
/// # Errors
/// Returns a version conflict or a typed database failure.
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
    state: crate::domain::price_book_entry::ReferenceState,
    reservation_id: Uuid,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::ReferenceState, Expr::value(state.as_str()))
        .col_expr(e::Column::ReservationId, Expr::value(reservation_id))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .filter(key(tenant, id).add(e::Column::Version.eq(version)))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("update price book entry reference".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}
/// Mark every `confirmed` or `lost` entry of `book` `released` (D-522: its book is archived), in
/// ONE statement: each gets a new version. The caller has read those entries and writes their
/// release ops in the same transaction.
/// # Errors
/// Returns typed database failures.
pub async fn release_book(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    book: Uuid,
    now: time::OffsetDateTime,
) -> Result<u64, RepoError> {
    use crate::domain::price_book_entry::ReferenceState;
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            e::Column::ReferenceState,
            Expr::value(ReferenceState::Released.as_str()),
        )
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::BookId.eq(book))
                .add(e::Column::ReferenceState.is_in([
                    ReferenceState::Confirmed.as_str(),
                    ReferenceState::Lost.as_str(),
                ])),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("release a book's price book entries".into(), e))?;
    Ok(result.rows_affected)
}
/// Delete an entry after the caller has removed its drafts, with the release op in the same transaction.
/// # Errors
/// Refuses a stale version or any remaining price; preserves database failures.
pub async fn delete_empty(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    version: i64,
) -> Result<(), RepoError> {
    use crate::infra::storage::entity::price;
    use toolkit_db::secure::SecureDeleteExt;
    let children = sea_orm::sea_query::Query::select()
        .expr(Expr::val(1))
        .from(price::Entity)
        .and_where(price::Column::TenantId.eq(tenant))
        .and_where(price::Column::PriceBookEntryId.eq(id))
        .to_owned();
    let result = e::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(
            key(tenant, id)
                .add(e::Column::Version.eq(version))
                .add(Expr::exists(children).not()),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete empty price book entry".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}

/// Bounded identity-ordered scan for the trusted reconciliation worker: confirmed entries,
/// whose receipts it checks, and lost entries, which it re-reserves once their SKU admits a
/// reservation again.
/// # Errors
/// Returns typed scoped storage failures.
pub async fn reconcile_batch(
    runner: &impl DBRunner,
    scope: &AccessScope,
    cursor: Option<Uuid>,
    limit: u64,
) -> Result<Vec<e::Model>, RepoError> {
    let mut filter = Condition::all().add(e::Column::ReferenceState.is_in(["confirmed", "lost"]));
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
        .map_err(|e| driver_failure("confirmed price book entry batch".into(), e))
}
#[cfg(test)]
#[path = "price_book_entry_repo_tests.rs"]
mod tests;
