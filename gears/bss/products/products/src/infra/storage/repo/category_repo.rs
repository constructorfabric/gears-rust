//! Flat category persistence; retirement and its reference check are one statement.
use super::{HeadWrite, driver_failure, map_unique};
use crate::domain::category::{CategoryPatch, NewCategory};
use crate::infra::storage::{
    RepoError, RepoRefusal,
    entity::{category, sku},
};
use bss_products_sdk::models::Category;
use sea_orm::sea_query::{Expr, ExprTrait, Query};
use sea_orm::{ColumnTrait, Condition, EntityTrait, FromQueryResult, Order, QuerySelect, Set};
use std::collections::HashMap;
use time::OffsetDateTime;
use toolkit_db::odata::sea_orm_filter::{
    FieldToColumn, LimitCfg, ODataFieldMapping, PaginateOdataTryError, paginate_odata_try,
};
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_odata::filter::{FieldKind, FilterField, FilterOp, ODataValue};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, Page, SortDir};
use uuid::Uuid;

fn category_of(m: category::Model) -> Category {
    Category {
        id: m.id,
        tenant_id: m.tenant_id,
        code: m.code,
        name: m.name,
        is_default: m.is_default,
        sort_order: m.sort_order,
        status: m.status,
        version: m.version,
        archived_at: m.archived_at,
        archived_by: m.archived_by,
    }
}
fn key(tenant: Uuid, id: Uuid) -> Condition {
    Condition::all()
        .add(category::Column::TenantId.eq(tenant))
        .add(category::Column::Id.eq(id))
}
/// Insert an active category.
/// # Errors
/// Returns category-code conflicts, `CATEGORY_DEFAULT_TAKEN` when another category is the default
/// (the door clears it first, P-D-218; this is a lost race), or scoped storage failures.
pub async fn insert_category(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    new: NewCategory,
    now: OffsetDateTime,
) -> Result<Category, RepoError> {
    let model = category::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        code: Set(new.code),
        name: Set(new.name),
        is_default: Set(new.is_default),
        sort_order: Set(new.sort_order),
        status: Set("active".into()),
        version: Set(1),
        created_at: Set(now),
        updated_at: Set(now),
        archived_at: Set(None),
        archived_by: Set(None),
    };
    category::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure("category scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map(category_of)
        .map_err(|e| map_unique("insert category".into(), e))
}
/// Read a category within the tenant and access scope.
/// # Errors
/// Returns scoped storage failures.
pub async fn find_category(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Option<Category>, RepoError> {
    category::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(key(tenant_id, id))
        .one(runner)
        .await
        .map(|m| m.map(category_of))
        .map_err(|e| driver_failure("find category".into(), e))
}
/// List categories by display order then stable code.
/// # Errors
/// Returns scoped storage failures.
pub async fn list_categories(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
) -> Result<Vec<Category>, RepoError> {
    category::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(category::Column::TenantId.eq(tenant_id)))
        .order_by(category::Column::SortOrder, Order::Asc)
        .order_by(category::Column::Code, Order::Asc)
        .all(runner)
        .await
        .map(|m| m.into_iter().map(category_of).collect())
        .map_err(|e| driver_failure("list categories".into(), e))
}
/// Patch a category only at the revision the caller saw.
/// # Errors
/// Returns `CATEGORY_DEFAULT_TAKEN` when `is_default: true` meets another default (the door
/// clears it first, P-D-218; this is a lost race), or scoped storage failures.
pub async fn update_category(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    expected_version: i64,
    patch: CategoryPatch,
    now: OffsetDateTime,
) -> Result<HeadWrite<Category>, RepoError> {
    let mut q = category::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            category::Column::Version,
            Expr::col(category::Column::Version).add(1_i64),
        )
        .col_expr(category::Column::UpdatedAt, Expr::value(now));
    if let Some(v) = patch.name {
        q = q.col_expr(category::Column::Name, Expr::value(v));
    }
    if let Some(v) = patch.is_default {
        q = q.col_expr(category::Column::IsDefault, Expr::value(v));
    }
    if let Some(v) = patch.sort_order {
        q = q.col_expr(category::Column::SortOrder, Expr::value(v));
    }
    let r = q
        .filter(key(tenant_id, id).add(category::Column::Version.eq(expected_version)))
        .exec(runner)
        .await
        .map_err(|e| map_unique("update category".into(), e))?;
    category_written(runner, scope, tenant_id, id, r.rows_affected).await
}
/// Clear the tenant's default when a category other than `keep` holds it (P-D-218): a move of the
/// default is this, then the write that sets it, in one transaction. Each cleared row gets a new
/// version, like any category write, and is returned for its audit row; the partial unique index
/// allows at most one. A default a concurrent move already cleared is not matched again.
/// # Errors
/// Returns scoped storage failures.
pub async fn clear_default_category(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    keep: Option<Uuid>,
    now: OffsetDateTime,
) -> Result<Vec<Category>, RepoError> {
    let mut held = Condition::all()
        .add(category::Column::TenantId.eq(tenant_id))
        .add(category::Column::IsDefault.eq(true));
    if let Some(keep) = keep {
        held = held.add(category::Column::Id.ne(keep));
    }
    category::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(category::Column::IsDefault, Expr::value(false))
        .col_expr(
            category::Column::Version,
            Expr::col(category::Column::Version).add(1_i64),
        )
        .col_expr(category::Column::UpdatedAt, Expr::value(now))
        .filter(held)
        .exec_with_returning(runner)
        .await
        .map(|rows| rows.into_iter().map(category_of).collect())
        .map_err(|e| driver_failure("clear the default category".into(), e))
}
/// Clear `id`'s default flag when it holds the tenant's default (P-D-220): retiring the default
/// clears it first, as a category write of its own (`version` + 1, `updated_at`), so the tenant is
/// left without a default. `None` when the category is not the default.
/// # Errors
/// Returns scoped storage failures.
pub async fn clear_default_of(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    now: OffsetDateTime,
) -> Result<Option<Category>, RepoError> {
    category::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(category::Column::IsDefault, Expr::value(false))
        .col_expr(
            category::Column::Version,
            Expr::col(category::Column::Version).add(1_i64),
        )
        .col_expr(category::Column::UpdatedAt, Expr::value(now))
        .filter(key(tenant_id, id).add(category::Column::IsDefault.eq(true)))
        .exec_with_returning(runner)
        .await
        .map(|rows| rows.into_iter().next().map(category_of))
        .map_err(|e| driver_failure("clear the retired default".into(), e))
}
async fn category_written(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    affected: u64,
) -> Result<HeadWrite<Category>, RepoError> {
    if affected == 0 {
        return Ok(HeadWrite::Unmatched);
    }
    find_category(runner, scope, tenant, id)
        .await?
        .map(HeadWrite::Written)
        .ok_or_else(|| RepoError::CorruptRow("written category disappeared".into()))
}
/// The SKU lifecycles that keep a category in use (P-D-208): every one but `retired`.
pub const CATEGORY_HOLDING_LIFECYCLES: [&str; 3] = ["draft", "published", "deprecated"];
/// Retire only an active, unused category; callers use a serializable transaction. A category is
/// in use while a SKU in `draft`, `published` or `deprecated` names it, including one whose retire
/// is in review (P-D-248: it keeps that lifecycle). A `retired` SKU no longer keeps it (P-D-208,
/// amending P-D-186). A SKU without a category (P-D-196) never matches
/// `category_id = <id>`, so it never keeps one in use.
/// # Errors
/// Returns scoped storage failures. Missing categories return `None`; an already retired or
/// in-use category is `Unmatched`, and the door re-reads to say which.
pub async fn retire_category_if_unused(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    now: OffsetDateTime,
) -> Result<Option<HeadWrite<Category>>, RepoError> {
    let used = Query::select()
        .expr(Expr::val(1))
        .from(sku::Entity)
        .and_where(sku::Column::TenantId.eq(tenant_id))
        .and_where(sku::Column::CategoryId.eq(id))
        .and_where(Expr::from(super::sku_repo::effective_lifecycle_in(
            crate::infra::storage::stored_now().date(),
            &CATEGORY_HOLDING_LIFECYCLES,
        )))
        .to_owned();
    let r = category::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(category::Column::Status, Expr::value("retired"))
        .col_expr(
            category::Column::Version,
            Expr::col(category::Column::Version).add(1_i64),
        )
        .col_expr(category::Column::UpdatedAt, Expr::value(now))
        .filter(
            key(tenant_id, id)
                .add(category::Column::Status.eq("active"))
                .add(Expr::exists(used).not()),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("retire category".into(), e))?;
    if r.rows_affected == 0 {
        return Ok(find_category(runner, scope, tenant_id, id)
            .await?
            .map(|_| HeadWrite::Unmatched));
    }
    category_written(runner, scope, tenant_id, id, r.rows_affected)
        .await
        .map(Some)
}
/// Write the archive mark (P-D-263) at the version the caller read: `Some(actor)` archives the
/// category now, `None` unarchives it. The mark is a write of its own (`version` + 1,
/// `updated_at`); the status is not touched, and the door judges which category may carry it.
/// # Errors
/// Returns scoped storage failures. `Unmatched` when the version moved or the category is gone.
pub async fn set_category_archived(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    expected_version: i64,
    archived_by: Option<Uuid>,
    now: OffsetDateTime,
) -> Result<HeadWrite<Category>, RepoError> {
    let r = category::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            category::Column::ArchivedAt,
            Expr::value(archived_by.map(|_| now)),
        )
        .col_expr(category::Column::ArchivedBy, Expr::value(archived_by))
        .col_expr(
            category::Column::Version,
            Expr::col(category::Column::Version).add(1_i64),
        )
        .col_expr(category::Column::UpdatedAt, Expr::value(now))
        .filter(key(tenant_id, id).add(category::Column::Version.eq(expected_version)))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("mark category archived".into(), e))?;
    category_written(runner, scope, tenant_id, id, r.rows_affected).await
}
/// The page size when the caller names none, and the most a page holds (P-D-215).
pub const CATEGORY_PAGE: LimitCfg = LimitCfg {
    default: 200,
    max: 200,
};

/// Every field of the category list's pager: what a `$filter` may name and what an `$orderby`
/// may name (the door publishes the two views).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CategoryListField {
    Id,
    Code,
    Name,
    Status,
    IsDefault,
    SortOrder,
    /// The archive mark (P-D-263): taken out of the `$filter` before the pager, never compared
    /// as a column.
    Archived,
}
impl FilterField for CategoryListField {
    const FIELDS: &'static [Self] = &[
        Self::Id,
        Self::Code,
        Self::Name,
        Self::Status,
        Self::IsDefault,
        Self::SortOrder,
        Self::Archived,
    ];
    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Code => "code",
            Self::Name => "name",
            Self::Status => "status",
            Self::IsDefault => "is_default",
            Self::SortOrder => "sort_order",
            Self::Archived => bss_rest::archived::ARCHIVED,
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::Id => FieldKind::Uuid,
            Self::Code | Self::Name | Self::Status => FieldKind::String,
            Self::IsDefault | Self::Archived => FieldKind::Bool,
            Self::SortOrder => FieldKind::I64,
        }
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}
impl CategoryListField {
    /// Whether the field may key an order and a cursor: `status`, `is_default` and `archived` are
    /// filter-only (two values each; an order by them says nothing a filter does not).
    #[must_use]
    pub const fn orderable(self) -> bool {
        matches!(self, Self::Id | Self::Code | Self::Name | Self::SortOrder)
    }
}
/// How the pager reads the category row for each field.
pub struct CategoryListMapping;
impl FieldToColumn<CategoryListField> for CategoryListMapping {
    type Column = category::Column;
    fn map_field(field: CategoryListField) -> category::Column {
        match field {
            CategoryListField::Id => category::Column::Id,
            CategoryListField::Code => category::Column::Code,
            CategoryListField::Name => category::Column::Name,
            CategoryListField::Status => category::Column::Status,
            CategoryListField::IsDefault => category::Column::IsDefault,
            CategoryListField::SortOrder => category::Column::SortOrder,
            CategoryListField::Archived => category::Column::ArchivedAt,
        }
    }
    /// No field is nullable (the toolkit's parser refuses `null` on each); `status` compares
    /// (`eq`, `ne`, `in`) with `active` or `retired` only.
    fn map_value(
        field: CategoryListField,
        op: FilterOp,
        value: &ODataValue,
    ) -> Result<ODataValue, String> {
        if field == CategoryListField::Archived {
            // P-D-263: the list takes its `archived` terms out first.
            return Err(bss_rest::archived::ARCHIVED_FILTER_REFUSED.to_owned());
        }
        if field == CategoryListField::Status
            && matches!(op, FilterOp::Eq | FilterOp::Ne | FilterOp::In)
            && !matches!(value, ODataValue::String(v) if v == "active" || v == "retired")
        {
            return Err(format!("unknown status: {value}"));
        }
        Ok(value.clone())
    }
    fn is_orderable(field: CategoryListField) -> bool {
        field.orderable()
    }
}
impl ODataFieldMapping<CategoryListField> for CategoryListMapping {
    type Entity = category::Entity;
    fn extract_cursor_value(model: &category::Model, field: CategoryListField) -> sea_orm::Value {
        match field {
            CategoryListField::Id => sea_orm::Value::Uuid(Some(model.id)),
            CategoryListField::Code => sea_orm::Value::String(Some(model.code.clone())),
            CategoryListField::Name => sea_orm::Value::String(Some(model.name.clone())),
            CategoryListField::Status => sea_orm::Value::String(Some(model.status.clone())),
            CategoryListField::IsDefault => sea_orm::Value::Bool(Some(model.is_default)),
            CategoryListField::SortOrder => sea_orm::Value::Int(Some(model.sort_order)),
            CategoryListField::Archived => sea_orm::Value::Bool(Some(model.archived_at.is_some())),
        }
    }
}

/// One page of the tenant's categories: the query's `$filter`, cursor and order — by default
/// `sort_order`, then `code` (P-D-215) — tie-broken by `id`; `$top` defaults to 200 and is clamped
/// there. An archived category is left out unless the filter asks `archived eq true` (P-D-263).
/// # Errors
/// [`super::SkuListError::Query`] for a value, order field or cursor the pager refuses;
/// [`super::SkuListError::Repo`] for storage.
pub async fn page_categories(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    query: &ODataQuery,
) -> Result<Page<Category>, super::SkuListError> {
    let mut query = query.clone();
    let (rest, archived) = super::take_archived(query.filter.take().map(|f| *f))
        .map_err(super::SkuListError::Query)?;
    query.filter = rest.map(Box::new);
    if query.cursor.is_none() && query.order.0.is_empty() {
        query.order = ODataOrderBy(
            [CategoryListField::SortOrder, CategoryListField::Code]
                .map(|field| OrderKey {
                    field: field.name().to_owned(),
                    dir: SortDir::Asc,
                })
                .to_vec(),
        );
    }
    let select = category::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(category::Column::TenantId.eq(tenant)))
        .filter(super::sku_list_repo::archive_mark(
            category::Column::ArchivedAt,
            archived,
        ));
    paginate_odata_try::<
        CategoryListField,
        CategoryListMapping,
        category::Entity,
        Category,
        _,
        RepoError,
        _,
    >(
        select,
        runner,
        &query,
        (CategoryListField::Id.name(), SortDir::Asc),
        CATEGORY_PAGE,
        |m| Ok(category_of(m)),
    )
    .await
    .map_err(|e| match e {
        // Kept as a driver failure so the retry classifier still reads the driver's message.
        PaginateOdataTryError::OData(toolkit_odata::Error::Db(message)) => {
            super::SkuListError::Repo(RepoError::Driver {
                context: "list categories".into(),
                source: sea_orm::DbErr::Custom(message),
            })
        }
        PaginateOdataTryError::OData(other) => super::SkuListError::Query(other),
        PaginateOdataTryError::MapError(e) => super::SkuListError::Repo(e),
    })
}

#[derive(Debug, FromQueryResult)]
struct CategoryCount {
    category_id: Uuid,
    n: i64,
}
/// How many SKUs that are not retired name each category (P-D-215) — the SKUs that keep it in use
/// (P-D-208) — in ONE grouped read: the tenant's categories when `category` is `None`, that one
/// otherwise. A category no such SKU names is absent from the map: its count is zero.
/// # Errors
/// Returns scoped storage failures; a negative count is a corrupt row.
pub async fn count_live_skus_by_category(
    runner: &impl DBRunner,
    tenant: Uuid,
    category: Option<Uuid>,
) -> Result<HashMap<Uuid, u64>, RepoError> {
    let mut c = Condition::all()
        .add(sku::Column::TenantId.eq(tenant))
        .add(sku::Column::CategoryId.is_not_null())
        .add(super::sku_repo::effective_lifecycle_in(
            crate::infra::storage::stored_now().date(),
            &CATEGORY_HOLDING_LIFECYCLES,
        ));
    if let Some(id) = category {
        c = c.add(sku::Column::CategoryId.eq(id));
    }
    sku::Entity::find()
        .secure()
        .scope_with(&AccessScope::for_tenant(tenant))
        .filter(c)
        .project_all(runner, |q| {
            q.select_only()
                .column(sku::Column::CategoryId)
                .column_as(Expr::col((sku::Entity, sku::Column::Id)).count(), "n")
                .group_by(sku::Column::CategoryId)
                .into_model::<CategoryCount>()
        })
        .await
        .map_err(|e| driver_failure("count category SKUs".into(), e))?
        .into_iter()
        .map(|row| {
            u64::try_from(row.n)
                .map(|n| (row.category_id, n))
                .map_err(|_| RepoError::CorruptRow(format!("a negative count {}", row.n)))
        })
        .collect()
}

/// Validate the category in the caller's serializable authoring transaction.
/// # Errors
/// Returns `CATEGORY_RETIRED` for an inactive category, or a missing-category refusal.
pub(crate) async fn require_active_category(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<(), RepoError> {
    match find_category(runner, scope, tenant, id).await? {
        Some(c) if c.status == "active" => Ok(()),
        Some(_) => Err(RepoError::Refused(RepoRefusal::CategoryRetired)),
        None => Err(RepoError::Refused(RepoRefusal::CategoryNotFound)),
    }
}
