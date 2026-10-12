//! Scoped plan persistence with conditional versions (D-394).
use super::{driver_failure, map_unique, matched};
use crate::infra::storage::{RepoError, entity::plan as e};
use sea_orm::sea_query::{Expr, ExprTrait, Func};
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, QuerySelect, Set};
use toolkit_db::odata::sea_orm_filter::{
    FieldToColumn, LimitCfg, ODataFieldMapping, PaginateOdataTryError, filter_node_to_condition,
    paginate_odata_try,
};
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_odata::filter::{FieldKind, FilterField, convert_expr_to_filter_node};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, Page, SortDir};
use uuid::Uuid;
fn key(tenant: Uuid, id: Uuid) -> Condition {
    Condition::all()
        .add(e::Column::TenantId.eq(tenant))
        .add(e::Column::Id.eq(id))
}
/// Insert a tenant-scoped plan in the caller's transaction.
/// # Errors
/// `PLAN_CODE_TAKEN`; database failures keep their type.
pub async fn insert(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<e::Model, RepoError> {
    let active = e::ActiveModel {
        id: Set(m.id),
        tenant_id: Set(m.tenant_id),
        code: Set(m.code),
        name: Set(m.name),
        published_rev: Set(m.published_rev),
        version: Set(m.version),
        created_by: Set(m.created_by),
        created_at: Set(m.created_at),
        updated_at: Set(m.updated_at),
        work_revision_id: Set(None),
        work_state: Set(None),
        scheduled_revision_id: Set(None),
        scheduled_from: Set(None),
        published_revision_id: Set(None),
        current_book_id: Set(None),
        current_currency: Set(None),
        last_activity_at: Set(m.updated_at),
    };
    let saved = e::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| driver_failure("insert plan scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map_err(|e| map_unique("insert plan".into(), e))?;
    super::plan_summary::refresh(runner, scope, m.tenant_id, saved.id).await?;
    find(runner, scope, m.tenant_id, saved.id)
        .await?
        .ok_or(RepoError::Conflict {
            code: "PLAN_NOT_FOUND",
        })
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
        .map_err(|e| driver_failure("find plan".into(), e))
}
/// The tenant's plans among `ids`, by id, in ONE statement whatever their number; an id the
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
        .map_err(|e| driver_failure("list plans by id".into(), e))
}
/// List the tenant's plans by code.
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
        .order_by(e::Column::Code, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list plans".into(), e))
}
/// The tenant's plans, by code, that have a draft, pending, scheduled or published revision whose
/// items name an entry of `sku` — the plans the SKU's usage counts (D-428, D-434) — in ONE
/// statement whatever their number. An included item without an entry names no entry and does not
/// count. The revisions, items and entries are read tenant-scoped.
/// # Errors
/// Returns typed database failures.
pub async fn naming_sku(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    sku: Uuid,
) -> Result<Vec<e::Model>, RepoError> {
    let naming = sku_revisions(tenant, sku);
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(Expr::exists(naming)),
        )
        .order_by(e::Column::Code, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list the plans naming a SKU".into(), e))
}
/// Rename at the version the caller read.
/// # Errors
/// A concurrent change is `STALE_REVISION`; database failures keep their type.
pub async fn rename(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    version: i64,
    name: String,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::Name, Expr::value(name))
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(key(tenant, id).add(e::Column::Version.eq(version)))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("rename plan".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")?;
    super::plan_summary::refresh(runner, scope, tenant, id).await
}
/// Write the revision number a revision's apply publishes, at the version the caller read.
/// # Errors
/// A concurrent change is `STALE_REVISION`; database failures keep their type.
pub async fn set_published(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    version: i64,
    published_rev: i32,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::PublishedRev, Expr::value(Some(published_rev)))
        .col_expr(e::Column::UpdatedAt, Expr::value(now))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(key(tenant, id).add(e::Column::Version.eq(version)))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("publish plan projection".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")?;
    super::plan_summary::refresh(runner, scope, tenant, id).await
}
/// Advance the revision number a due switch publishes (D-448), in the caller's transaction.
/// `published_rev` is a projection of the revisions, not an edit of the plan: neither the plan's
/// `version` nor its `updated_at` moves, so an If-Match read before the switch stays good and a
/// read that derives the switch (D-447) shows the same plan row as one after it (plan rev 2 L6).
/// # Errors
/// `PLAN_NOT_FOUND` for a plan the tenant does not hold; database failures keep their type.
pub async fn advance_published(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    published_rev: i32,
) -> Result<(), RepoError> {
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::PublishedRev, Expr::value(Some(published_rev)))
        .filter(key(tenant, id))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("advance the plan's published projection".into(), e))?;
    matched(result.rows_affected, "PLAN_NOT_FOUND")
}
/// Delete a plan that was never published and has no revision left, at the version the caller
/// read, in the caller's transaction (D-417): its code is free again.
/// # Errors
/// A published plan, a remaining revision or a lost version is `STALE_REVISION`; database
/// failures keep their type.
pub async fn delete_unpublished(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    version: i64,
) -> Result<(), RepoError> {
    use crate::infra::storage::entity::plan_revision;
    use toolkit_db::secure::SecureDeleteExt;
    let revisions = sea_orm::sea_query::Query::select()
        .expr(Expr::val(1))
        .from(plan_revision::Entity)
        .and_where(plan_revision::Column::TenantId.eq(tenant))
        .and_where(plan_revision::Column::PlanId.eq(id))
        .to_owned();
    let result = e::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(
            key(tenant, id)
                .add(e::Column::Version.eq(version))
                .add(e::Column::PublishedRev.is_null())
                .add(Expr::exists(revisions).not()),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete unpublished plan".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}

/// Revisions whose items name `sku` through an entry, correlated to the plan row (D-434).
fn sku_revisions(tenant: Uuid, sku: Uuid) -> sea_orm::sea_query::SelectStatement {
    use crate::domain::plan::RevisionState;
    use crate::infra::storage::entity::{
        plan_item as item, plan_revision as revision, price_book_entry as entry,
    };
    sea_orm::sea_query::Query::select()
        .expr(Expr::val(1))
        .from(revision::Entity)
        .inner_join(
            item::Entity,
            Expr::col((item::Entity, item::Column::RevisionId))
                .equals((revision::Entity, revision::Column::Id)),
        )
        .inner_join(
            entry::Entity,
            Expr::col((entry::Entity, entry::Column::Id))
                .equals((item::Entity, item::Column::PriceBookEntryId)),
        )
        .and_where(
            Expr::col((revision::Entity, revision::Column::PlanId))
                .equals((e::Entity, e::Column::Id)),
        )
        .and_where(Expr::col((revision::Entity, revision::Column::TenantId)).eq(tenant))
        .and_where(
            Expr::col((revision::Entity, revision::Column::State))
                .ne(RevisionState::Superseded.as_str()),
        )
        .and_where(Expr::col((item::Entity, item::Column::TenantId)).eq(tenant))
        .and_where(Expr::col((entry::Entity, entry::Column::TenantId)).eq(tenant))
        .and_where(Expr::col((entry::Entity, entry::Column::SkuId)).eq(sku))
        .to_owned()
}

/// `selling` as 1 or 0 from the stored columns and the request's day (D-484).
fn selling_flag(today: time::Date) -> Expr {
    Expr::case(
        Condition::any()
            .add(e::Column::PublishedRevisionId.is_not_null())
            .add(
                Condition::all()
                    .add(e::Column::ScheduledFrom.is_not_null())
                    .add(e::Column::ScheduledFrom.lte(today)),
            ),
        Expr::val(1),
    )
    .finally(Expr::val(0))
    .into()
}

/// `change` from the stored columns and the request's day (D-484).
fn change_expr(today: time::Date) -> Expr {
    Func::coalesce([
        Expr::col((e::Entity, e::Column::WorkState)),
        Expr::case(e::Column::ScheduledFrom.gt(today), Expr::val("scheduled"))
            .finally(Expr::val("none"))
            .into(),
    ])
    .into()
}

/// The page size when the caller names none, and the most a page holds (D-485). Past 500 plans a
/// caller follows `next_cursor`.
pub const PLAN_PAGE: LimitCfg = LimitCfg {
    default: 500,
    max: 500,
};

/// Fields of the plans pager. `book_id` and `currency` are the current revision's. `id` is the
/// tie-break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlanListField {
    Id,
    Code,
    Name,
    BookId,
    Currency,
    LastActivityAt,
}
impl FilterField for PlanListField {
    const FIELDS: &'static [Self] = &[
        Self::Id,
        Self::Code,
        Self::Name,
        Self::BookId,
        Self::Currency,
        Self::LastActivityAt,
    ];
    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Code => "code",
            Self::Name => "name",
            Self::BookId => "book_id",
            Self::Currency => "currency",
            Self::LastActivityAt => "last_activity_at",
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::Id | Self::BookId => FieldKind::Uuid,
            Self::Code | Self::Name | Self::Currency => FieldKind::String,
            Self::LastActivityAt => FieldKind::DateTimeUtc,
        }
    }
    fn nullable(&self) -> bool {
        matches!(self, Self::BookId | Self::Currency)
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}
/// The `$orderby` fields. `book_id` and `currency` filter but do not order (D-485).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlanOrderField {
    Id,
    Code,
    Name,
    LastActivityAt,
}
impl FilterField for PlanOrderField {
    const FIELDS: &'static [Self] = &[Self::Id, Self::Code, Self::Name, Self::LastActivityAt];
    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Code => "code",
            Self::Name => "name",
            Self::LastActivityAt => "last_activity_at",
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::Id => FieldKind::Uuid,
            Self::Code | Self::Name => FieldKind::String,
            Self::LastActivityAt => FieldKind::DateTimeUtc,
        }
    }
    fn nullable(&self) -> bool {
        false
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS
            .iter()
            .copied()
            .find(|field| field.name() == name)
    }
}
impl PlanListField {
    #[must_use]
    pub const fn orderable(self) -> bool {
        matches!(
            self,
            Self::Id | Self::Code | Self::Name | Self::LastActivityAt
        )
    }
    fn column(self) -> e::Column {
        match self {
            Self::Id => e::Column::Id,
            Self::Code => e::Column::Code,
            Self::Name => e::Column::Name,
            Self::BookId => e::Column::CurrentBookId,
            Self::Currency => e::Column::CurrentCurrency,
            Self::LastActivityAt => e::Column::LastActivityAt,
        }
    }
}
/// How the pager reads a plan row.
pub struct PlanListMapping;
impl FieldToColumn<PlanListField> for PlanListMapping {
    type Column = e::Column;
    fn map_field(field: PlanListField) -> e::Column {
        field.column()
    }
    fn is_orderable(field: PlanListField) -> bool {
        field.orderable()
    }
}
impl ODataFieldMapping<PlanListField> for PlanListMapping {
    type Entity = e::Entity;
    fn extract_cursor_value(model: &e::Model, field: PlanListField) -> sea_orm::Value {
        match field {
            PlanListField::Id => sea_orm::Value::Uuid(Some(model.id)),
            PlanListField::Code => sea_orm::Value::String(Some(model.code.clone())),
            PlanListField::Name => sea_orm::Value::String(Some(model.name.clone())),
            PlanListField::BookId => sea_orm::Value::Uuid(model.current_book_id),
            PlanListField::Currency => sea_orm::Value::String(model.current_currency.clone()),
            PlanListField::LastActivityAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(model.last_activity_at))
            }
        }
    }
}

/// What the list narrows by, besides `$filter`.
#[derive(Debug, Clone)]
pub struct PlanListFilter {
    pub text: Option<String>,
    pub sku: Option<Uuid>,
    pub selling: Option<bool>,
    /// Empty: every change. Otherwise the derived `change` is one of these tokens.
    pub change: Vec<String>,
    pub today: time::Date,
}

/// A list read refused or failed.
#[derive(Debug)]
pub enum PlanListError {
    Query(toolkit_odata::Error),
    Repo(RepoError),
}

fn text_condition(text: &str, backend: sea_orm::DbBackend) -> Condition {
    super::text_like_any(
        text,
        backend,
        [
            Expr::col((e::Entity, e::Column::Code)),
            Expr::col((e::Entity, e::Column::Name)),
        ],
    )
}

fn narrowed(tenant: Uuid, backend: sea_orm::DbBackend, filter: &PlanListFilter) -> Condition {
    let mut c = Condition::all().add(e::Column::TenantId.eq(tenant));
    if let Some(text) = filter.text.as_deref() {
        c = c.add(text_condition(text, backend));
    }
    if let Some(sku) = filter.sku {
        c = c.add(Expr::exists(sku_revisions(tenant, sku)));
    }
    if let Some(selling) = filter.selling {
        c = c.add(selling_flag(filter.today).eq(i32::from(selling)));
    }
    if !filter.change.is_empty() {
        c = c.add(change_expr(filter.today).is_in(filter.change.clone()));
    }
    c
}

fn map_page_error(error: PaginateOdataTryError<RepoError>) -> PlanListError {
    match error {
        PaginateOdataTryError::OData(toolkit_odata::Error::Db(message)) => {
            PlanListError::Repo(RepoError::Driver {
                context: "list plans".into(),
                source: sea_orm::DbErr::Custom(message),
            })
        }
        PaginateOdataTryError::OData(other) => PlanListError::Query(other),
        PaginateOdataTryError::MapError(error) => PlanListError::Repo(error),
    }
}

/// One page of the tenant's plans (D-485). The day-dependent axes are predicates on the stored
/// summary. `$top` defaults to 500 and is clamped at 500. Tie-break `id` follows the order.
/// # Errors
/// [`PlanListError::Query`] for a value, order field or cursor the pager refuses;
/// [`PlanListError::Repo`] for storage.
pub async fn page(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    backend: sea_orm::DbBackend,
    filter: &PlanListFilter,
    query: &ODataQuery,
) -> Result<Page<e::Model>, PlanListError> {
    let mut query = query.clone();
    if query.cursor.is_none() && query.order.0.is_empty() {
        query.order = ODataOrderBy(vec![OrderKey {
            field: PlanListField::Code.name().to_owned(),
            dir: SortDir::Asc,
        }]);
    }
    let tie = query.order.0.first().map_or(SortDir::Asc, |key| key.dir);
    let select = e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(narrowed(tenant, backend, filter));
    paginate_odata_try::<PlanListField, PlanListMapping, e::Entity, e::Model, _, RepoError, _>(
        select,
        runner,
        &query,
        (PlanListField::Id.name(), tie),
        PLAN_PAGE,
        Ok,
    )
    .await
    .map_err(map_page_error)
}

/// The counts under the same narrowing, one grouped statement (D-485).
/// # Errors
/// A `$filter` the list refuses, or storage.
pub async fn count(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    backend: sea_orm::DbBackend,
    filter: &PlanListFilter,
    query: &ODataQuery,
) -> Result<PlanCounts, PlanListError> {
    #[derive(Debug, sea_orm::FromQueryResult)]
    struct Bucket {
        /// The `selling_flag` CASE of two integer literals: INT4 on Postgres, INTEGER on SQLite.
        selling: i32,
        change: String,
        n: i64,
    }
    let mut condition = narrowed(tenant, backend, filter);
    if let Some(ast) = query.filter.as_deref() {
        let node = convert_expr_to_filter_node::<PlanListField>(ast).map_err(|e| {
            PlanListError::Query(toolkit_odata::Error::InvalidFilter(e.to_string()))
        })?;
        condition = condition.add(
            filter_node_to_condition::<PlanListField, PlanListMapping>(&node)
                .map_err(toolkit_odata::Error::InvalidFilter)
                .map_err(PlanListError::Query)?,
        );
    }
    let selling = selling_flag(filter.today);
    let change = change_expr(filter.today);
    let rows = e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(condition)
        .project_all(runner, |q| {
            q.select_only()
                .expr_as(selling.clone(), "selling")
                .expr_as(change.clone(), "change")
                .column_as(e::Column::Id.count(), "n")
                // By position: each expression binds `today`, and Postgres does not match a
                // SELECT expression to a GROUP BY one whose parameters are separate binds (42803).
                .group_by(Expr::cust("1"))
                .group_by(Expr::cust("2"))
                .into_model::<Bucket>()
        })
        .await
        .map_err(|e| PlanListError::Repo(driver_failure("count plans".into(), e)))?;
    let mut counts = PlanCounts::default();
    for row in rows {
        let n = u64::try_from(row.n).map_err(|_| {
            PlanListError::Repo(RepoError::CorruptRow(format!("plan count {}", row.n)))
        })?;
        counts.total = counts.total.saturating_add(n);
        if row.selling == 0 {
            counts.selling_false = counts.selling_false.saturating_add(n);
        } else {
            counts.selling_true = counts.selling_true.saturating_add(n);
        }
        // One change comes back once per selling value, so each bucket adds its rows.
        let bucket = match row.change.as_str() {
            "none" => &mut counts.none,
            "draft" => &mut counts.draft,
            "pending" => &mut counts.pending,
            "scheduled" => &mut counts.scheduled,
            other => {
                return Err(PlanListError::Repo(RepoError::CorruptRow(format!(
                    "plan change {other:?}"
                ))));
            }
        };
        *bucket = bucket.saturating_add(n);
    }
    Ok(counts)
}

/// One grouped count of the list's narrowing (D-485).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PlanCounts {
    pub selling_true: u64,
    pub selling_false: u64,
    pub none: u64,
    pub draft: u64,
    pub pending: u64,
    pub scheduled: u64,
    pub total: u64,
}
