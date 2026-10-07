//! SKU heads, conditional locks and local reference fences.
//! @cpt-dod:cpt-cf-bss-products-dod-unit-store:p1
use super::{HeadWrite, category_repo::require_active_category, driver_failure, map_unique};
use crate::domain::derived;
use crate::domain::sku::{LifecycleHead, NewSku, effective_lifecycle};
use crate::infra::storage::{
    RepoError,
    entity::{sku, sku_reference},
};
use bss_products_sdk::models::{BillingTiming, Lifecycle, LifecycleNext, Sku, SkuContent, SkuType};
use sea_orm::sea_query::{Expr, ExprTrait, Query, SimpleExpr};
use sea_orm::{
    ColumnTrait, Condition, DbBackend, EntityTrait, Order, QueryOrder, QuerySelect, Set,
};
use time::{Date, OffsetDateTime};
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use uuid::Uuid;

fn key(tenant: Uuid, id: Uuid) -> Condition {
    Condition::all()
        .add(sku::Column::TenantId.eq(tenant))
        .add(sku::Column::Id.eq(id))
}
/// The lifecycle in force on `today` (P-D-249): `lifecycle_next` once `lifecycle_next_from` has
/// arrived, otherwise the stored `lifecycle`. One expression for every SQL predicate.
pub(crate) fn effective_lifecycle_expr(today: Date) -> SimpleExpr {
    Expr::case(
        Expr::col((sku::Entity, sku::Column::LifecycleNextFrom)).lte(today),
        Expr::col((sku::Entity, sku::Column::LifecycleNext)),
    )
    .finally(Expr::col((sku::Entity, sku::Column::Lifecycle)))
    .into()
}

/// The same rule as [`effective_lifecycle_expr`], as an OR the planner can seek: a due
/// `lifecycle_next`, or the stored `lifecycle` when the next has not arrived. The counts
/// projection keeps the `CASE`, because Postgres treats two copies of it as different
/// expressions in `GROUP BY`.
pub(crate) fn effective_lifecycle_in<T: AsRef<str>>(today: Date, tokens: &[T]) -> Condition {
    let mut any = Condition::any();
    for token in tokens {
        let token = token.as_ref();
        let due = Condition::all()
            .add(sku::Column::LifecycleNextFrom.is_not_null())
            .add(sku::Column::LifecycleNextFrom.lte(today))
            .add(sku::Column::LifecycleNext.eq(token));
        let stored = Condition::all()
            .add(
                Condition::any()
                    .add(sku::Column::LifecycleNextFrom.is_null())
                    .add(sku::Column::LifecycleNextFrom.gt(today)),
            )
            .add(sku::Column::Lifecycle.eq(token));
        any = any.add(Condition::any().add(due).add(stored));
    }
    any
}

/// Effective lifecycle other than `token`.
pub(crate) fn effective_lifecycle_ne(today: Date, token: &str) -> Condition {
    effective_lifecycle_in(today, &[token]).not()
}
fn today() -> Date {
    crate::infra::storage::stored_now().date()
}
fn lifecycle_in_force(row: &sku::Model) -> Result<Lifecycle, RepoError> {
    let stored = Lifecycle::parse(&row.lifecycle)
        .ok_or_else(|| RepoError::CorruptRow(format!("SKU lifecycle {}", row.lifecycle)))?;
    let next = scheduled_next(
        row.id,
        row.lifecycle_next.as_deref(),
        row.lifecycle_next_from,
    )?;
    Ok(effective_lifecycle(
        LifecycleHead {
            lifecycle: stored,
            next,
        },
        today(),
    ))
}

/// Both columns null, or both set. A half-set pair is a corrupt row (P-D-249).
fn scheduled_next(
    id: Uuid,
    next: Option<&str>,
    from: Option<Date>,
) -> Result<Option<LifecycleNext>, RepoError> {
    let next = next
        .map(|token| {
            Lifecycle::parse(token)
                .ok_or_else(|| RepoError::CorruptRow(format!("SKU {id} lifecycle_next {token}")))
        })
        .transpose()?;
    match (next, from) {
        (None, None) => Ok(None),
        (Some(lifecycle), Some(from)) => Ok(Some(LifecycleNext { lifecycle, from })),
        _ => Err(RepoError::CorruptRow(format!(
            "SKU {id} stores only half of lifecycle_next"
        ))),
    }
}
/// Fold a due `lifecycle_next` into `lifecycle` before a head write, so the write's predicate
/// sees the lifecycle in force (P-D-249). A read never depends on this.
async fn fold_due(
    runner: &impl DBRunner,
    scope: &AccessScope,
    filter: Condition,
) -> Result<(), RepoError> {
    sku::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            sku::Column::Lifecycle,
            Expr::col(sku::Column::LifecycleNext),
        )
        .col_expr(sku::Column::LifecycleNext, Expr::value(None::<String>))
        .col_expr(sku::Column::LifecycleNextFrom, Expr::value(None::<Date>))
        .filter(
            filter
                .add(sku::Column::LifecycleNext.is_not_null())
                .add(sku::Column::LifecycleNextFrom.lte(today())),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("fold SKU lifecycle".into(), e))?;
    Ok(())
}
async fn fold_head(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<(), RepoError> {
    fold_due(runner, scope, key(tenant, id)).await
}
pub(crate) fn sku_of(m: sku::Model) -> Result<Sku, RepoError> {
    let stored = Lifecycle::parse(&m.lifecycle)
        .ok_or_else(|| RepoError::CorruptRow(format!("SKU lifecycle {}", m.lifecycle)))?;
    let next = scheduled_next(m.id, m.lifecycle_next.as_deref(), m.lifecycle_next_from)?;
    let day = today();
    let head = LifecycleHead {
        lifecycle: stored,
        next,
    };
    let lifecycle_next = next.filter(|scheduled| scheduled.from > day);
    Ok(Sku {
        id: m.id,
        tenant_id: m.tenant_id,
        code: m.code,
        name: m.name,
        r#type: SkuType::parse(&m.r#type)
            .ok_or_else(|| RepoError::CorruptRow(format!("SKU type {}", m.r#type)))?,
        category_id: m.category_id,
        description: m.description,
        sellable: m.sellable,
        lifecycle: effective_lifecycle(head, day),
        retire_pending: m.retire_pending,
        lifecycle_next,
        revision: m.revision,
        published_version: m.published_version,
        gl_code: m.gl_code,
        tax_category: m.tax_category,
        invoice_line_template: m.invoice_line_template,
        billing_timing: m
            .billing_timing
            .map(|v| {
                BillingTiming::parse(&v)
                    .ok_or_else(|| RepoError::CorruptRow(format!("billing timing {v}")))
            })
            .transpose()?,
        usage_type_ref: m.usage_type_ref,
        unit: m.unit,
        type_change_pending: m.type_change_pending,
        pending_unit_id: m.pending_unit_id,
        approved_by_unit_id: m.approved_by_unit_id,
        created_by: m.created_by,
        created_at: m.created_at,
        updated_at: m.updated_at,
        archived_at: m.archived_at,
        archived_by: m.archived_by,
    })
}

/// Serve each SKU's unit (P-D-259). A derived usage SKU takes its version's `output_unit`, from
/// one read of those versions. A raw usage SKU keeps the unit stored on the row. A non-usage SKU
/// serves null.
///
/// # Errors
/// Scoped storage failures of the version read.
pub(crate) async fn fill_served_units(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    skus: &mut [Sku],
) -> Result<(), RepoError> {
    let mut meters = Vec::new();
    for sku in skus.iter() {
        if sku.r#type != SkuType::Usage {
            continue;
        }
        if let Some(reference) = sku
            .usage_type_ref
            .as_ref()
            .filter(|reference| derived::is_derived_ref(reference))
        {
            meters.push(reference.clone());
        }
    }
    meters.sort();
    meters.dedup();
    // PROBE-9-13-6: one read of the distinct versions, for a page of 10 and of 100.
    let mut units = std::collections::HashMap::new();
    for meter in &meters {
        units.extend(
            super::derived_usage_type_repo::output_units(
                runner,
                &scope.tenant_only(),
                tenant_id,
                std::slice::from_ref(meter),
            )
            .await?,
        );
    }
    for sku in skus.iter_mut() {
        if sku.r#type != SkuType::Usage {
            sku.unit = None;
            continue;
        }
        if let Some(reference) = sku
            .usage_type_ref
            .as_deref()
            .filter(|reference| derived::is_derived_ref(reference))
        {
            sku.unit = units.get(reference).cloned();
        }
    }
    Ok(())
}
/// A SKU without a category (P-D-196) has none to resolve; a given one must be the tenant's and
/// active.
async fn require_category(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Option<Uuid>,
) -> Result<(), RepoError> {
    match id {
        Some(id) => require_active_category(runner, scope, tenant, id).await,
        None => Ok(()),
    }
}
/// Insert a draft, under an active category when it names one, in the caller's serializable
/// transaction.
/// # Errors
/// Returns unique-code/name, category, or scoped storage errors.
pub async fn insert_sku(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    new: NewSku,
    created_by: Uuid,
    now: OffsetDateTime,
) -> Result<Sku, RepoError> {
    require_category(runner, scope, tenant_id, new.category_id).await?;
    let unit = derived::persisted_unit(new.usage_type_ref.as_deref(), new.unit);
    let model = sku::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        code: Set(new.code),
        name: Set(new.name),
        r#type: Set(new.r#type.as_str().into()),
        category_id: Set(new.category_id),
        description: Set(new.description),
        sellable: Set(new.sellable),
        lifecycle: Set("draft".into()),
        fenced_at: Set(None),
        fence_op_id: Set(None),
        revision: Set(1),
        published_version: Set(0),
        gl_code: Set(new.gl_code),
        tax_category: Set(new.tax_category),
        invoice_line_template: Set(new.invoice_line_template),
        billing_timing: Set(new.billing_timing.map(|v| v.as_str().to_owned())),
        usage_type_ref: Set(new.usage_type_ref),
        unit: Set(unit),
        type_change_pending: Set(false),
        retire_pending: Set(false),
        lifecycle_next: Set(None),
        lifecycle_next_from: Set(None),
        pending_unit_id: Set(None),
        approved_by_unit_id: Set(None),
        created_by: Set(created_by),
        created_at: Set(now),
        updated_at: Set(now),
        archived_at: Set(None),
        archived_by: Set(None),
    };
    let row = sku::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure("SKU scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map_err(|e| map_unique("insert SKU".into(), e))?;
    let sku = sku_of(row)?;
    let mut rows = vec![sku];
    fill_served_units(runner, scope, tenant_id, &mut rows).await?;
    Ok(rows.remove(0))
}
/// Find the tenant's visible SKU.
/// # Errors
/// Returns scoped storage or corrupt-row errors.
pub async fn find_sku(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Option<Sku>, RepoError> {
    let mut found = sku::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(key(tenant_id, id))
        .one(runner)
        .await
        .map_err(|e| driver_failure("find SKU".into(), e))?
        .map(sku_of)
        .transpose()?;
    if let Some(sku) = found.as_mut() {
        let rows = std::slice::from_mut(sku);
        fill_served_units(runner, scope, tenant_id, rows).await?;
    }
    Ok(found)
}
/// The tenant's SKUs among `ids`, in ONE statement whatever their number (P-D-245). An id the
/// tenant does not hold, or the scope does not admit, has no row. The caller orders them.
///
/// # Errors
/// Returns scoped storage or corrupt-row errors.
pub async fn find_skus(
    runner: &impl DBRunner,
    backend: DbBackend,
    scope: &AccessScope,
    tenant_id: Uuid,
    ids: &[Uuid],
) -> Result<Vec<Sku>, RepoError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut found = sku::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(sku::Column::TenantId.eq(tenant_id))
                .add(super::sku_list_repo::membership(backend, ids)),
        )
        .all(runner)
        .await
        .map_err(|e| driver_failure("find SKUs".into(), e))?
        .into_iter()
        .map(sku_of)
        .collect::<Result<Vec<_>, _>>()?;
    fill_served_units(runner, scope, tenant_id, &mut found).await?;
    Ok(found)
}
/// The browse catalog's filters and its exclusive code cursor; one extra row signals another
/// page. The operator's SKU list pages through [`super::page_skus`] (P-D-210).
#[derive(Debug, Clone)]
pub struct SkuQuery {
    /// Additional validated catalog predicate, composed inside the tenant scope.
    pub catalog_filter: Option<Condition>,
    pub lifecycle: Option<Lifecycle>,
    pub limit: u64,
    pub after_code: Option<String>,
}
/// List matching SKUs in stable code order (the browse catalog's read).
/// # Errors
/// Returns scoped storage or corrupt-row errors.
pub async fn list_skus(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    q: &SkuQuery,
) -> Result<Vec<Sku>, RepoError> {
    let mut c = Condition::all().add(sku::Column::TenantId.eq(tenant_id));
    if let Some(filter) = &q.catalog_filter {
        c = c.add(filter.clone());
    }
    if let Some(v) = q.lifecycle {
        c = c.add(effective_lifecycle_in(today(), &[v.as_str()]));
    }
    if let Some(v) = &q.after_code {
        c = c.add(sku::Column::Code.gt(v));
    }
    let mut found = sku::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(c)
        .order_by(sku::Column::Code, Order::Asc)
        .limit(q.limit.saturating_add(1))
        .all(runner)
        .await
        .map_err(|e| driver_failure("list SKUs".into(), e))?
        .into_iter()
        .map(sku_of)
        .collect::<Result<Vec<_>, _>>()?;
    fill_served_units(runner, scope, tenant_id, &mut found).await?;
    Ok(found)
}
#[derive(Debug, sea_orm::FromQueryResult)]
struct TaxCategoryRow {
    tax_category: String,
}
/// The distinct tax categories the tenant's published SKUs carry, in code order, in ONE projected
/// read (RS-14): the dictionary the browse door serves, without reading a SKU row.
/// # Errors
/// Returns scoped storage failures.
pub async fn distinct_tax_categories(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
) -> Result<Vec<String>, RepoError> {
    Ok(sku::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(sku::Column::TenantId.eq(tenant_id))
                .add(effective_lifecycle_in(
                    today(),
                    &[Lifecycle::Published.as_str()],
                ))
                .add(sku::Column::TaxCategory.is_not_null()),
        )
        .project_all(runner, |q| {
            q.select_only()
                .column(sku::Column::TaxCategory)
                .distinct()
                .order_by(sku::Column::TaxCategory, Order::Asc)
                .into_model::<TaxCategoryRow>()
        })
        .await
        .map_err(|e| driver_failure("distinct tax categories".into(), e))?
        .into_iter()
        .map(|row| row.tax_category)
        .collect())
}
fn content_update(
    scope: &AccessScope,
    c: &SkuContent,
    now: OffsetDateTime,
) -> toolkit_db::secure::SecureUpdateMany<sku::Entity, toolkit_db::secure::Scoped> {
    sku::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(sku::Column::Code, Expr::value(c.code.clone()))
        .col_expr(sku::Column::Name, Expr::value(c.name.clone()))
        .col_expr(sku::Column::Type, Expr::value(c.r#type.as_str()))
        .col_expr(sku::Column::CategoryId, Expr::value(c.category_id))
        .col_expr(sku::Column::Description, Expr::value(c.description.clone()))
        .col_expr(sku::Column::Sellable, Expr::value(c.sellable))
        .col_expr(sku::Column::GlCode, Expr::value(c.gl_code.clone()))
        .col_expr(
            sku::Column::TaxCategory,
            Expr::value(c.tax_category.clone()),
        )
        .col_expr(
            sku::Column::InvoiceLineTemplate,
            Expr::value(c.invoice_line_template.clone()),
        )
        .col_expr(
            sku::Column::BillingTiming,
            Expr::value(c.billing_timing.map(BillingTiming::as_str)),
        )
        .col_expr(
            sku::Column::UsageTypeRef,
            Expr::value(c.usage_type_ref.clone()),
        )
        .col_expr(
            sku::Column::Unit,
            Expr::value(derived::persisted_unit(
                c.usage_type_ref.as_deref(),
                c.unit.clone(),
            )),
        )
        .col_expr(
            sku::Column::Revision,
            Expr::col(sku::Column::Revision).add(1_i64),
        )
        .col_expr(sku::Column::UpdatedAt, Expr::value(now))
}
async fn written(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    affected: u64,
) -> Result<HeadWrite<Sku>, RepoError> {
    if affected == 0 {
        return Ok(HeadWrite::Unmatched);
    }
    find_sku(runner, scope, tenant, id)
        .await?
        .map(HeadWrite::Written)
        .ok_or_else(|| RepoError::CorruptRow("written SKU disappeared".into()))
}
/// Write content only to an unlocked draft at the observed revision.
/// # Errors
/// Returns category, unique-key or scoped storage errors.
pub async fn update_sku_draft(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    expected_revision: i64,
    content: &SkuContent,
    now: OffsetDateTime,
) -> Result<HeadWrite<Sku>, RepoError> {
    require_category(runner, scope, tenant_id, content.category_id).await?;
    fold_head(runner, scope, tenant_id, id).await?;
    let r = content_update(scope, content, now)
        .filter(
            key(tenant_id, id)
                .add(sku::Column::Lifecycle.eq("draft"))
                .add(sku::Column::Revision.eq(expected_revision))
                .add(sku::Column::PendingUnitId.is_null()),
        )
        .exec(runner)
        .await
        .map_err(|e| map_unique("update draft".into(), e))?;
    written(runner, scope, tenant_id, id, r.rows_affected).await
}
/// Apply approved business content, incrementing revision and published version.
/// # Errors
/// Returns category, unique-key or scoped storage errors; a missing head is corrupt.
pub async fn write_sku_content(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    content: &SkuContent,
    now: OffsetDateTime,
) -> Result<Sku, RepoError> {
    require_category(runner, scope, tenant_id, content.category_id).await?;
    fold_head(runner, scope, tenant_id, id).await?;
    let r = content_update(scope, content, now)
        .col_expr(
            sku::Column::PublishedVersion,
            Expr::col(sku::Column::PublishedVersion).add(1_i64),
        )
        .filter(key(tenant_id, id))
        .exec(runner)
        .await
        .map_err(|e| map_unique("apply SKU content".into(), e))?;
    match written(runner, scope, tenant_id, id, r.rows_affected).await? {
        HeadWrite::Written(s) => Ok(s),
        HeadWrite::Unmatched => Err(RepoError::CorruptRow("approval SKU missing".into())),
    }
}
/// Transition an approval-owned head only from the supplied states.
/// # Errors
/// Returns scoped storage failures.
pub async fn set_lifecycle(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    from: &[Lifecycle],
    to: Lifecycle,
    now: OffsetDateTime,
) -> Result<HeadWrite<Sku>, RepoError> {
    fold_head(runner, scope, tenant_id, id).await?;
    let r = sku::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(sku::Column::Lifecycle, Expr::value(to.as_str()))
        .col_expr(sku::Column::LifecycleNext, Expr::value(None::<String>))
        .col_expr(sku::Column::LifecycleNextFrom, Expr::value(None::<Date>))
        .col_expr(sku::Column::UpdatedAt, Expr::value(now))
        .col_expr(
            sku::Column::Revision,
            Expr::col(sku::Column::Revision).add(1_i64),
        )
        .filter(
            key(tenant_id, id).add(sku::Column::Lifecycle.is_in(from.iter().map(|v| v.as_str()))),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("set SKU lifecycle".into(), e))?;
    written(runner, scope, tenant_id, id, r.rows_affected).await
}
/// Store a lifecycle that takes effect on `on`, leaving the head's lifecycle until that day
/// (P-D-249). `from` is the lifecycle in force, which the fold has written first.
/// # Errors
/// Returns scoped storage failures. `Unmatched` when the head is not in `from`.
#[expect(
    clippy::too_many_arguments,
    reason = "the dated lifecycle write names the head, the states it may leave, the target and the date"
)]
pub async fn set_lifecycle_next(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    from: &[Lifecycle],
    target: Lifecycle,
    on: Date,
    now: OffsetDateTime,
) -> Result<HeadWrite<Sku>, RepoError> {
    fold_head(runner, scope, tenant_id, id).await?;
    let r = sku::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(sku::Column::LifecycleNext, Expr::value(target.as_str()))
        .col_expr(sku::Column::LifecycleNextFrom, Expr::value(on))
        .col_expr(sku::Column::UpdatedAt, Expr::value(now))
        .col_expr(
            sku::Column::Revision,
            Expr::col(sku::Column::Revision).add(1_i64),
        )
        .filter(
            key(tenant_id, id).add(sku::Column::Lifecycle.is_in(from.iter().map(|v| v.as_str()))),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("set SKU lifecycle next".into(), e))?;
    written(runner, scope, tenant_id, id, r.rows_affected).await
}
/// Drop a pending lifecycle change (an undo, or a retire that clears it).
/// # Errors
/// Returns scoped storage failures.
pub async fn clear_lifecycle_next(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    now: OffsetDateTime,
) -> Result<HeadWrite<Sku>, RepoError> {
    fold_head(runner, scope, tenant_id, id).await?;
    let r = sku::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(sku::Column::LifecycleNext, Expr::value(None::<String>))
        .col_expr(sku::Column::LifecycleNextFrom, Expr::value(None::<Date>))
        .col_expr(sku::Column::UpdatedAt, Expr::value(now))
        .col_expr(
            sku::Column::Revision,
            Expr::col(sku::Column::Revision).add(1_i64),
        )
        .filter(key(tenant_id, id))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("clear SKU lifecycle next".into(), e))?;
    written(runner, scope, tenant_id, id, r.rows_affected).await
}
/// The two operations that exclude every live local reference.
#[derive(Debug, Clone, Copy)]
pub enum Fence {
    Retire,
    TypeChange,
}
/// Fence and check live references in one write; the caller uses serializable isolation.
/// # Errors
/// Returns scoped storage failures.
pub async fn fence_sku(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    kind: Fence,
    op_id: Uuid,
    now: OffsetDateTime,
) -> Result<HeadWrite<Sku>, RepoError> {
    fold_head(runner, scope, tenant_id, id).await?;
    let live = Query::select()
        .expr(Expr::val(1))
        .from(sku_reference::Entity)
        .and_where(sku_reference::Column::TenantId.eq(tenant_id))
        .and_where(sku_reference::Column::SkuId.eq(id))
        .and_where(sku_reference::Column::State.ne("released"))
        .to_owned();
    let mut q = sku::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(sku::Column::FencedAt, Expr::value(now))
        .col_expr(sku::Column::FenceOpId, Expr::value(op_id));
    q = match kind {
        Fence::Retire => q.col_expr(sku::Column::RetirePending, Expr::value(true)),
        Fence::TypeChange => q.col_expr(sku::Column::TypeChangePending, Expr::value(true)),
    };
    let r = q
        .filter(
            key(tenant_id, id)
                .add(effective_lifecycle_in(
                    today(),
                    &["published", "deprecated"],
                ))
                .add(sku::Column::RetirePending.eq(false))
                .add(sku::Column::PendingUnitId.is_null())
                .add(sku::Column::FencedAt.is_null())
                .add(sku::Column::TypeChangePending.eq(false))
                .add(Expr::exists(live).not()),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("fence SKU".into(), e))?;
    written(runner, scope, tenant_id, id, r.rows_affected).await
}
/// `retired` is the moment an approved retirement takes effect: the head turns `retired`, and as a
/// change of the row it moves `revision`, the concurrency version its `ETag` names, and `updated_at`.
fn clear_fence(
    scope: &AccessScope,
    retired: Option<OffsetDateTime>,
) -> toolkit_db::secure::SecureUpdateMany<sku::Entity, toolkit_db::secure::Scoped> {
    let mut q = sku::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(sku::Column::TypeChangePending, Expr::value(false))
        .col_expr(sku::Column::RetirePending, Expr::value(false))
        .col_expr(sku::Column::FencedAt, Expr::value(None::<OffsetDateTime>))
        .col_expr(sku::Column::FenceOpId, Expr::value(None::<Uuid>));
    if let Some(now) = retired {
        q = q
            .col_expr(sku::Column::Lifecycle, Expr::value("retired"))
            .col_expr(sku::Column::LifecycleNext, Expr::value(None::<String>))
            .col_expr(sku::Column::LifecycleNextFrom, Expr::value(None::<Date>))
            .col_expr(sku::Column::UpdatedAt, Expr::value(now))
            .col_expr(
                sku::Column::Revision,
                Expr::col(sku::Column::Revision).add(1_i64),
            );
    }
    q
}
/// Operator/TTL release cannot touch a fence held by a pending unit.
/// # Errors
/// Returns scoped storage failures.
pub async fn unfence_sku(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    op_id: Option<Uuid>,
) -> Result<HeadWrite<Sku>, RepoError> {
    fold_head(runner, scope, tenant_id, id).await?;
    let mut c = key(tenant_id, id).add(sku::Column::PendingUnitId.is_null());
    if let Some(op) = op_id {
        c = c.add(sku::Column::FenceOpId.eq(op));
    }
    let r = clear_fence(scope, None)
        .filter(c)
        .exec(runner)
        .await
        .map_err(|e| driver_failure("unfence SKU".into(), e))?;
    written(runner, scope, tenant_id, id, r.rows_affected).await
}
/// Release the subject's lock and matching fence atomically.
/// # Errors
/// Returns scoped storage failures.
#[expect(
    clippy::too_many_arguments,
    reason = "the compare-and-swap operands of the lock and the fence stay explicit"
)]
pub async fn unlock_and_unfence(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    unit_id: Uuid,
    op_id: Uuid,
    approved_by: Option<Uuid>,
    retired: Option<OffsetDateTime>,
) -> Result<HeadWrite<Sku>, RepoError> {
    fold_head(runner, scope, tenant_id, id).await?;
    let mut q =
        clear_fence(scope, retired).col_expr(sku::Column::PendingUnitId, Expr::value(None::<Uuid>));
    if let Some(approved_by) = approved_by {
        q = q.col_expr(sku::Column::ApprovedByUnitId, Expr::value(approved_by));
    }
    let r = q
        .filter(
            key(tenant_id, id)
                .add(sku::Column::PendingUnitId.eq(unit_id))
                .add(sku::Column::FenceOpId.eq(op_id)),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("unlock and unfence SKU".into(), e))?;
    written(runner, scope, tenant_id, id, r.rows_affected).await
}
/// Acquire a pending-unit lock only on the observed, unlocked revision.
/// # Errors
/// Returns scoped storage failures.
pub async fn try_lock_sku(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    unit_id: Uuid,
    expected_revision: i64,
) -> Result<bool, RepoError> {
    fold_head(runner, scope, tenant_id, id).await?;
    let r = sku::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(sku::Column::PendingUnitId, Expr::value(unit_id))
        .filter(
            key(tenant_id, id)
                .add(sku::Column::PendingUnitId.is_null())
                .add(sku::Column::Revision.eq(expected_revision)),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("lock SKU".into(), e))?;
    Ok(r.rows_affected == 1)
}
/// Release a unit lock, preserving prior approval attribution unless replaced.
/// # Errors
/// Returns scoped storage failures.
pub async fn unlock_sku(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    unit_id: Uuid,
    approved_by: Option<Uuid>,
) -> Result<HeadWrite<Sku>, RepoError> {
    fold_head(runner, scope, tenant_id, id).await?;
    let mut q = sku::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(sku::Column::PendingUnitId, Expr::value(None::<Uuid>));
    if let Some(v) = approved_by {
        q = q.col_expr(sku::Column::ApprovedByUnitId, Expr::value(v));
    }
    let result = q
        .filter(key(tenant_id, id).add(sku::Column::PendingUnitId.eq(unit_id)))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("unlock SKU".into(), e))?;
    written(runner, scope, tenant_id, id, result.rows_affected).await
}
/// Count every head that names a category, retired heads included. Since P-D-208 a retired head no
/// longer keeps a category in use (`retire_category_if_unused`); a SKU without a category
/// (P-D-196) never matches `category_id = <id>`.
/// # Errors
/// Returns scoped storage failures.
pub async fn count_skus_in_category(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    category_id: Uuid,
) -> Result<u64, RepoError> {
    sku::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(sku::Column::TenantId.eq(tenant_id))
                .add(sku::Column::CategoryId.eq(category_id)),
        )
        .count(runner)
        .await
        .map_err(|e| driver_failure("count category SKUs".into(), e))
}
/// Delete a never-published, unlocked draft at the revision the caller read (P-D-206), in the
/// caller's transaction. Only the head row goes: its audit rows are append-only and stay, and a
/// draft owns no version row (`published_version = 0`) and admits no reservation. The door checks
/// the registry for a row naming the SKU in the same transaction.
/// # Errors
/// Returns scoped storage failures. `false` when no row matched: absent, foreign, no longer a
/// never-published draft, locked or at another revision; the door re-reads to say which.
pub async fn delete_draft_sku(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    expected_revision: i64,
) -> Result<bool, RepoError> {
    use toolkit_db::secure::SecureDeleteExt;
    fold_head(runner, scope, tenant_id, id).await?;
    let r = sku::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(
            key(tenant_id, id)
                .add(sku::Column::Lifecycle.eq("draft"))
                .add(sku::Column::PublishedVersion.eq(0_i64))
                .add(sku::Column::PendingUnitId.is_null())
                .add(sku::Column::Revision.eq(expected_revision)),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete draft SKU".into(), e))?;
    Ok(r.rows_affected == 1)
}
/// Write the archive mark (P-D-263) at the revision the caller read: `Some(actor)` archives the
/// SKU now, `None` unarchives it. The mark is a write of its own (`revision` + 1, `updated_at`); the
/// lifecycle is not touched, and the door judges which SKU may carry the mark.
/// # Errors
/// Returns scoped storage failures. `Unmatched` when the revision moved or the SKU is gone.
pub async fn set_sku_archived(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    expected_revision: i64,
    archived_by: Option<Uuid>,
    now: OffsetDateTime,
) -> Result<HeadWrite<Sku>, RepoError> {
    let r = sku::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            sku::Column::ArchivedAt,
            Expr::value(archived_by.map(|_| now)),
        )
        .col_expr(sku::Column::ArchivedBy, Expr::value(archived_by))
        .col_expr(sku::Column::UpdatedAt, Expr::value(now))
        .col_expr(
            sku::Column::Revision,
            Expr::col(sku::Column::Revision).add(1_i64),
        )
        .filter(key(tenant_id, id).add(sku::Column::Revision.eq(expected_revision)))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("mark SKU archived".into(), e))?;
    written(runner, scope, tenant_id, id, r.rows_affected).await
}
#[cfg(test)]
#[path = "sku_repo_tests.rs"]
mod sku_repo_tests;

/// The typed lifecycle of a fence row (RS-63), so a fence check compares [`Lifecycle`] variants
/// and a typo cannot compile. A stored token outside the set is a corrupt row.
/// # Errors
/// `CorruptRow` naming the SKU and the token.
pub fn fence_lifecycle(row: &sku::Model) -> Result<Lifecycle, RepoError> {
    Lifecycle::parse(&row.lifecycle)
        .ok_or_else(|| RepoError::CorruptRow(format!("SKU {} lifecycle {}", row.id, row.lifecycle)))
}
/// Read private fence ownership without putting it in business snapshots.
/// # Errors
/// Returns scoped storage failures.
pub async fn find_sku_fence(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Option<sku::Model>, RepoError> {
    sku::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(key(tenant_id, id))
        .one(runner)
        .await
        .map_err(|e| driver_failure("find SKU fence".into(), e))
}

/// An orphan fence the maintenance expiry lifted (P-D-189): the SKU, the lifecycle it had while
/// fenced, the one it returned to, and its revision — what the expiry's audit row records
/// (P-D-213).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpiredFence {
    pub id: Uuid,
    pub from: Lifecycle,
    pub to: Lifecycle,
    pub revision: i64,
}
/// Lift one orphan fence at the operation that holds it, reporting the move when it lifted one.
async fn lift(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    fenced: &sku::Model,
) -> Result<Option<ExpiredFence>, RepoError> {
    let from = lifecycle_in_force(fenced)?;
    Ok(
        match unfence_sku(runner, scope, tenant, fenced.id, fenced.fence_op_id).await? {
            HeadWrite::Written(s) => Some(ExpiredFence {
                id: s.id,
                from,
                to: s.lifecycle,
                revision: s.revision,
            }),
            HeadWrite::Unmatched => None,
        },
    )
}
/// Expire one SKU's orphan fence once it is older than `ttl_minutes`: never a fence a pending unit
/// holds, and only at the operation observed (P-D-189). The caller writes the audit row.
/// # Errors
/// Returns scoped storage failures and a stored lifecycle outside the five.
pub async fn expire_orphan_fence(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    cutoff: OffsetDateTime,
) -> Result<Option<ExpiredFence>, RepoError> {
    match find_sku_fence(runner, scope, tenant, id).await? {
        Some(row)
            if row.pending_unit_id.is_none() && row.fenced_at.is_some_and(|at| at <= cutoff) =>
        {
            lift(runner, scope, tenant, &row).await
        }
        _ => Ok(None),
    }
}

/// Recover tenant-scoped orphan fences before applying list filters, set-based whatever their
/// number (P-D-211): one read of the tenant's fences older than `cutoff` (the lifecycle each had
/// while fenced, which the lift overwrites), then ONE `UPDATE … RETURNING` lifting them all. Both
/// run in the caller's transaction on the same predicate, so the lift writes exactly the fences
/// read, each at the operation observed (P-D-189). Pending units cannot be released, even when
/// their fences are old. Answers what it lifted, for the caller's audit rows (P-D-213); when it
/// finds none, the read is its only statement.
/// # Errors
/// Returns scoped storage failures and a stored lifecycle outside the five.
pub async fn expire_orphan_fences(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    cutoff: OffsetDateTime,
) -> Result<Vec<ExpiredFence>, RepoError> {
    let orphan = || {
        Condition::all()
            .add(sku::Column::TenantId.eq(tenant))
            .add(sku::Column::PendingUnitId.is_null())
            .add(sku::Column::FencedAt.lte(cutoff))
    };
    let fenced = sku::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(orphan())
        .all(runner)
        .await
        .map_err(|e| driver_failure("find orphan SKU fences".into(), e))?;
    if fenced.is_empty() {
        return Ok(Vec::new());
    }
    fold_due(runner, scope, orphan()).await?;
    let mut found = std::collections::HashMap::with_capacity(fenced.len());
    for row in &fenced {
        found.insert(row.id, lifecycle_in_force(row)?);
    }
    let mut lifted = clear_fence(scope, None)
        .filter(orphan())
        .exec_with_returning(runner)
        .await
        .map_err(|e| driver_failure("lift orphan SKU fences".into(), e))?;
    lifted.sort_by_key(|row| row.id);
    lifted
        .into_iter()
        .map(|row| {
            let from = found.get(&row.id).copied().ok_or_else(|| {
                RepoError::Db(format!(
                    "orphan fence {} lifted without being read in its transaction",
                    row.id
                ))
            })?;
            let to = Lifecycle::parse(&row.lifecycle)
                .ok_or_else(|| RepoError::CorruptRow(format!("SKU lifecycle {}", row.lifecycle)))?;
            Ok(ExpiredFence {
                id: row.id,
                from,
                to,
                revision: row.revision,
            })
        })
        .collect()
}
