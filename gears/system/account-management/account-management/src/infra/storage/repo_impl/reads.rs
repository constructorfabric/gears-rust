//! Read-only repo methods over `tenants` + `tenant_closure`:
//! `find_by_id`, `list_children`, `count_children`, `is_descendant`.
//! None of these mutate state; all are scope-checked against the
//! caller's [`AccessScope`] except `is_descendant` which answers a
//! structural closure question and intentionally bypasses the per-row
//! scope (PEP gate is the service-layer guard).

use std::collections::HashMap;

use account_management_sdk::TenantInfoFilterField;
use bigdecimal::BigDecimal;
use gts::GtsId;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, FromQueryResult, Order, QuerySelect};
use serde_json::Value;
use toolkit_db::odata::sea_orm_filter::{
    FieldToColumn, LimitCfg, ODataFieldMapping, PaginateOdataTryError, paginate_odata_try,
};
use toolkit_db::secure::SecureEntityExt;
use toolkit_odata::filter::{FieldKind, FilterField as _, FilterOp, ODataValue};
use toolkit_odata::{ODataQuery, Page, SortDir, ast};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::tenant::model::{ChildCountFilter, TenantModel, TenantStatus};
use crate::infra::storage::entity::{tenant_closure, tenant_idp_metadata, tenants};

use super::TenantRepoImpl;
use super::helpers::{entity_to_model, id_eq, map_scope_err};

/// `OData` mapper for `tenants`. Maps the public SDK filter fields
/// ([`TenantInfoFilterField`]) onto the underlying `SeaORM` columns
/// and surfaces cursor values for `paginate_odata`'s tiebreaker
/// logic. Mirrors the
/// [`super::metadata::MetadataODataMapper`] pattern.
struct TenantODataMapper;

impl FieldToColumn<TenantInfoFilterField> for TenantODataMapper {
    type Column = tenants::Column;

    fn map_field(field: TenantInfoFilterField) -> tenants::Column {
        match field {
            TenantInfoFilterField::Id => tenants::Column::Id,
            TenantInfoFilterField::Name => tenants::Column::Name,
            TenantInfoFilterField::Status => tenants::Column::Status,
            // Both the raw-UUID filter and the chained-string filter target
            // the same storage column; the string form is mapped to its
            // UUIDv5 in `map_value` below.
            TenantInfoFilterField::TenantTypeUuid | TenantInfoFilterField::TenantType => {
                tenants::Column::TenantTypeUuid
            }
            TenantInfoFilterField::SelfManaged => tenants::Column::SelfManaged,
            TenantInfoFilterField::CreatedAt => tenants::Column::CreatedAt,
            TenantInfoFilterField::UpdatedAt => tenants::Column::UpdatedAt,
        }
    }

    /// Translate the SDK-facing `status` string contract into the
    /// storage-side numeric value. Wire callers speak the public
    /// [`account_management_sdk::TenantStatus`] strings
    /// (`"active"` / `"suspended"` / `"deleted"`); the column on
    /// disk is `SMALLINT` with the encoding pinned by
    /// [`TenantStatus::as_smallint`]. The hook keeps the storage
    /// encoding out of the SDK contract: unknown strings — including
    /// the AM-internal `"provisioning"` — surface as a validation
    /// error before the predicate reaches `SeaORM`.
    ///
    /// Only the membership-style operators (`Eq` / `Ne` / `In`) are
    /// admissible on `status`: an ordered comparison
    /// (`status lt 'deleted'`) would otherwise be rewritten to
    /// `status < 3` and start comparing the hidden storage ordinal
    /// instead of the published wire strings. Tenant lifecycle is a
    /// categorical column, so there is no honest meaning for ordered
    /// operators on either shape — the mapper rejects them.
    ///
    /// Other fields fall through to the default identity
    /// implementation.
    fn map_value(
        field: TenantInfoFilterField,
        op: FilterOp,
        value: &ODataValue,
    ) -> Result<ODataValue, String> {
        match (field, value) {
            (TenantInfoFilterField::Status, ODataValue::String(s)) => {
                match op {
                    FilterOp::Eq | FilterOp::Ne | FilterOp::In => {}
                    other => {
                        return Err(format!(
                            "operator {other:?} is not supported on `status`; \
                             use `eq`, `ne`, or `in` — ordered comparisons on a \
                             categorical lifecycle column would silently fall \
                             back to the storage ordinal"
                        ));
                    }
                }
                let code = match s.as_str() {
                    "active" => TenantStatus::Active.as_smallint(),
                    "suspended" => TenantStatus::Suspended.as_smallint(),
                    "deleted" => TenantStatus::Deleted.as_smallint(),
                    other => {
                        return Err(format!(
                            "invalid `status` value '{other}'; expected one of \
                             'active', 'suspended', 'deleted'"
                        ));
                    }
                };
                Ok(ODataValue::Number(BigDecimal::from(i64::from(code))))
            }
            // Chained `gts.*` tenant-type string → its deterministic UUIDv5,
            // compared against the `tenant_type_uuid` column. Same derivation
            // as `uuid_for_registered_schema` (`GtsId::try_new(s).to_uuid()`), so a
            // value taken from the projection round-trips to the stored UUID.
            // Only membership operators are admissible — an ordered comparison
            // on a derived UUID has no honest meaning (mirrors `status`).
            (TenantInfoFilterField::TenantType, ODataValue::String(s)) => {
                match op {
                    FilterOp::Eq | FilterOp::Ne | FilterOp::In => {}
                    other => {
                        return Err(format!(
                            "operator {other:?} is not supported on `tenant_type`; \
                             use `eq`, `ne`, or `in` — ordered comparisons on a \
                             derived UUIDv5 have no meaningful order"
                        ));
                    }
                }
                let uuid = GtsId::try_new(s).map(|g| g.to_uuid()).map_err(|e| {
                    format!(
                        "invalid `tenant_type` value '{s}'; expected a chained GTS \
                         type id (e.g. 'gts.cf.core.am.tenant_type.v1~…~'): {e}"
                    )
                })?;
                Ok(ODataValue::Uuid(uuid))
            }
            _ => Ok(value.clone()),
        }
    }

    /// `$orderby=status` sorts by the lifecycle ordinal (`Active = 1 <
    /// Suspended = 2 < Deleted = 3`), NOT alphabetically by the wire
    /// string — a deliberate categorical order (healthy first,
    /// tombstones last, and inverted via `desc`) that operator UIs sort
    /// their Status column by. Cursor pages are safe: both
    /// the effective-order comparison and `extract_cursor_value` speak
    /// the storage `SMALLINT`, so the codec never sees a
    /// translated-shape value.
    ///
    /// `$orderby=tenant_type` stays rejected: it compares via a derived
    /// `UUIDv5`, and an ordered comparison on a derived UUID has no
    /// honest meaning.
    fn is_orderable(field: TenantInfoFilterField) -> bool {
        !matches!(field, TenantInfoFilterField::TenantType)
    }
}

impl ODataFieldMapping<TenantInfoFilterField> for TenantODataMapper {
    type Entity = tenants::Entity;

    fn extract_cursor_value(
        model: &tenants::Model,
        field: TenantInfoFilterField,
    ) -> sea_orm::Value {
        match field {
            TenantInfoFilterField::Id => sea_orm::Value::Uuid(Some(Box::new(model.id))),
            TenantInfoFilterField::Name => {
                sea_orm::Value::String(Some(Box::new(model.name.clone())))
            }
            TenantInfoFilterField::Status => sea_orm::Value::SmallInt(Some(model.status)),
            // `TenantType` is filter-only (not orderable), so it never reaches
            // the cursor path; map it identically to the raw-UUID field for
            // exhaustiveness.
            TenantInfoFilterField::TenantTypeUuid | TenantInfoFilterField::TenantType => {
                sea_orm::Value::Uuid(Some(Box::new(model.tenant_type_uuid)))
            }
            TenantInfoFilterField::SelfManaged => sea_orm::Value::Bool(Some(model.self_managed)),
            TenantInfoFilterField::CreatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(Box::new(model.created_at)))
            }
            TenantInfoFilterField::UpdatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(Box::new(model.updated_at)))
            }
        }
    }

    /// `status` is a wire string but sorts (and therefore cursors) by
    /// its storage lifecycle ordinal — encode/parse its cursor token as
    /// an integer, not with the wire `String` kind. Every
    /// other field's cursor shape matches its wire kind.
    fn cursor_kind(field: TenantInfoFilterField) -> FieldKind {
        match field {
            TenantInfoFilterField::Status => FieldKind::I64,
            other => other.kind(),
        }
    }
}

/// Pagination limits for the tenant-children listing surface. Mirrors
/// [`super::metadata::METADATA_LIMIT_CFG`] so the platform-wide cap is
/// uniform across AM listing surfaces (`default = 50`,
/// `max = 200`). The gear-config `listing.max_top` accessor remains
/// for future REST handlers that want to surface the per-deployment
/// cap, but the repo seam itself uses this constant to defend against
/// builders that forget to clamp.
const TENANT_LISTING_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 50,
    max: 200,
};

/// Recursively scan an `$filter` AST for any reference to a named
/// column. Used by [`list_children`] to detect whether the caller has
/// supplied a `status` predicate; if not, the repo ANDs the AM-default
/// `status IN (Active, Suspended)` hidden-AND so soft-deleted rows
/// stay invisible by default.
#[allow(
    clippy::match_same_arms,
    reason = "And/Or and Compare arms share the same recursive body but are deliberately kept on separate match arms for AST-semantic clarity — boolean composition (`And` / `Or`) and a leaf-comparison (`Compare`) are semantically distinct flows of the filter walker, and forcing them into one OR-pattern would obscure that"
)]
fn filter_references_field(expr: &ast::Expr, field: &str) -> bool {
    match expr {
        // Exact-match leaf: production filter columns (declared by
        // `TenantInfoQuery`) are flat identifiers; the OData parser
        // does not produce slash-joined property paths for any
        // currently-supported `FieldKind`, so an `Identifier` matches
        // the column name verbatim. If sub-navigation ever lands on
        // the SDK surface (e.g. `tenant_type/name`), this leaf check
        // becomes a `name == field || name.starts_with(&format!("{field}/"))`.
        ast::Expr::Identifier(name) => name == field,
        ast::Expr::And(l, r) | ast::Expr::Or(l, r) => {
            filter_references_field(l, field) || filter_references_field(r, field)
        }
        ast::Expr::Compare(l, _, r) => {
            filter_references_field(l, field) || filter_references_field(r, field)
        }
        ast::Expr::Not(inner) => filter_references_field(inner, field),
        ast::Expr::In(l, items) => {
            filter_references_field(l, field)
                || items.iter().any(|i| filter_references_field(i, field))
        }
        ast::Expr::Function(_, args) => args.iter().any(|a| filter_references_field(a, field)),
        ast::Expr::Value(_) => false,
    }
}

pub(super) async fn find_by_id(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Option<TenantModel>, DomainError> {
    let conn = repo.db.conn()?;
    let row = tenants::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(id_eq(id))
        .one(&conn)
        .await
        .map_err(map_scope_err)?;
    match row {
        Some(r) => Ok(Some(entity_to_model(r)?)),
        None => Ok(None),
    }
}

/// Load the opaque plugin-private metadata blob AM stamped at
/// `activate_tenant` time. Returns `None` when no row exists for
/// `tenant_id`, or when the row's `metadata` column is SQL NULL
/// (plugin reported no per-tenant state).
///
/// AM never interprets the JSON shape; the value flows straight
/// into [`account_management_sdk::IdpTenantContext::metadata`] on the
/// next `IdP` call for this tenant.
pub(super) async fn find_idp_metadata(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    tenant_id: Uuid,
) -> Result<Option<Value>, DomainError> {
    let conn = repo.db.conn()?;
    let row = tenant_idp_metadata::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(tenant_idp_metadata::Column::TenantId.eq(tenant_id)))
        .one(&conn)
        .await
        .map_err(map_scope_err)?;
    Ok(row.and_then(|r| r.metadata))
}

pub(super) async fn find_many(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    ids: &[Uuid],
) -> Result<Vec<TenantModel>, DomainError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // Deduplicate caller-supplied ids so the resulting `IN (...)` clause
    // does not re-query the same row on its behalf.
    let mut deduped: Vec<Uuid> = ids.to_vec();
    deduped.sort_unstable();
    deduped.dedup();

    let conn = repo.db.conn()?;
    let rows = tenants::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(tenants::Column::Id.is_in(deduped))
                .add(tenants::Column::DeletedAt.is_null()),
        )
        // Stable id-asc ordering so the returned Vec matches the
        // already-sorted-deduped `deduped` input layout. Without an
        // explicit `ORDER BY`, callers that zip / pair against the
        // sorted input (or that rely on deterministic test output)
        // see engine-dependent row order on Postgres.
        .order_by(tenants::Column::Id, Order::Asc)
        .all(&conn)
        .await
        .map_err(map_scope_err)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(entity_to_model(r)?);
    }
    Ok(out)
}

pub(super) async fn list_children(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    parent_id: Uuid,
    query: &ODataQuery,
) -> Result<Page<TenantModel>, DomainError> {
    let conn = repo.db.conn()?;

    // Base filter: parent_id pin + provisioning-exclusion + optional
    // hidden-AND status default. The OData `$filter` (over the SDK-
    // declared filter columns) is applied on top by `paginate_odata`.
    //
    // Hidden-AND default: when the caller has not mentioned `status`
    // in `$filter`, AND the base condition with `status IN (Active,
    // Suspended)` so soft-deleted rows stay invisible by default.
    // Callers wanting to see deleted rows pass
    // `$filter=status eq 'deleted'` explicitly (the string form is the
    // SDK contract; the impl-side `TenantODataMapper::map_value` hook
    // translates it into the storage SMALLINT before binding). This
    // preserves the legacy `ListChildrenQuery::status_filter = None
    // -> Active+Suspended only` contract.
    let mut base_cond = Condition::all()
        .add(tenants::Column::ParentId.eq(parent_id))
        // Defence-in-depth: `Provisioning` rows never cross the public
        // listing boundary; the service layer also retains a final
        // post-page filter on `is_sdk_visible` for the same reason.
        .add(tenants::Column::Status.ne(TenantStatus::Provisioning.as_smallint()));

    let caller_filters_status = query
        .filter()
        .is_some_and(|ast| filter_references_field(ast, "status"));
    if !caller_filters_status {
        base_cond = base_cond.add(
            Condition::any()
                .add(tenants::Column::Status.eq(TenantStatus::Active.as_smallint()))
                .add(tenants::Column::Status.eq(TenantStatus::Suspended.as_smallint())),
        );
    }

    let base = tenants::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(base_cond);

    // Cursor stability:
    //
    // * The unique tiebreaker passed to `paginate_odata` is
    //   `id ASC` — the primary key. Using a column with a UNIQUE
    //   constraint guarantees the effective order is a total order,
    //   so the cursor predicate `(a, b) > (a0, b0)` cannot silently
    //   skip rows on a duplicate-key collision (e.g. two siblings
    //   sharing a `created_at` microsecond on batch INSERT).
    //
    // * Chronological default — when the caller supplies no
    //   `$orderby`, we inject `created_at ASC` into `query.order`
    //   here (not via the tiebreaker, which is reserved for the
    //   unique key). `ensure_tiebreaker` inside `paginate_odata`
    //   then appends `id ASC`, yielding effective order
    //   `(created_at ASC, id ASC)` — the legacy chronological
    //   default plus a UNIQUE tiebreaker.
    //
    // * Cursor pages — when a cursor is present, `paginate_odata`
    //   re-derives the effective order from the cursor's signed
    //   tokens, so the injection here is skipped (the helper
    //   ignores `query.order` on cursor pages).
    let query = if query.cursor.is_none() && query.order.is_empty() {
        let mut adjusted = query.clone();
        adjusted.order = adjusted.order.ensure_tiebreaker("created_at", SortDir::Asc);
        std::borrow::Cow::Owned(adjusted)
    } else {
        std::borrow::Cow::Borrowed(query)
    };
    // `paginate_odata_try` because `entity_to_model` is fallible —
    // a `tenants` row with an out-of-domain `status` SMALLINT or a
    // negative `depth` (structurally pinned by DDL `CHECK` +
    // column-type but theoretically reachable via legacy / manually-
    // repaired databases) surfaces as `DomainError::Internal` (HTTP
    // 500) rather than panicking the worker. The fallible variant
    // shares the cursor / filter / ordering machinery with the
    // plain `paginate_odata` — only the `model → domain` projection
    // step is allowed to fail per-row.
    let page = paginate_odata_try::<TenantInfoFilterField, TenantODataMapper, _, _, _, _, _>(
        base,
        &conn,
        query.as_ref(),
        ("id", SortDir::Asc),
        TENANT_LISTING_LIMIT_CFG,
        entity_to_model,
    )
    .await
    .map_err(|e| match e {
        PaginateOdataTryError::OData(odata_err) => DomainError::Validation {
            detail: format!("list_children query rejected: {odata_err}"),
        },
        // Caller's domain error (`Internal { diagnostic, cause }`)
        // is preserved verbatim — the canonical envelope at the AM
        // boundary maps it to HTTP 500 with the drift diagnostic
        // payload so operators see the bad row identifier.
        PaginateOdataTryError::MapError(domain_err) => domain_err,
    })?;

    Ok(page)
}

pub(super) async fn count_children(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    parent_id: Uuid,
    filter: ChildCountFilter,
) -> Result<u64, DomainError> {
    let connection = repo.db.conn()?;
    let mut sql_filter = Condition::all().add(tenants::Column::ParentId.eq(parent_id));
    if matches!(filter, ChildCountFilter::NonDeleted) {
        sql_filter =
            sql_filter.add(tenants::Column::Status.ne(TenantStatus::Deleted.as_smallint()));
    }
    tenants::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(sql_filter)
        .count(&connection)
        .await
        .map_err(map_scope_err)
}

/// Direct-child counts for a batch of parent ids, keyed by parent.
///
/// Powers the public `child_count` read-shape field (`list_children`
/// page rows + single-tenant reads). The semantics are deliberately
/// different from [`count_children`], which is an internal
/// delete-saga guard (`allow_all`, always counts `Provisioning`):
///
/// * **Scope-filtered** — bounded by the caller's `scope` through
///   `SecureORM`, so direct children behind a self-managed barrier the
///   caller cannot penetrate collapse to `0`. This matches the
///   visibility rule the rest of the public read surface obeys.
/// * **`Provisioning` excluded** — those rows have no public
///   representation anywhere on the SDK boundary. `Deleted` rows are
///   *included*, mirroring that they stay reachable via
///   `$filter=status eq 'deleted'`.
///
/// One grouped `COUNT ... GROUP BY parent_id` for the whole batch — no
/// N+1 across the page. Parents with no matching child are absent from
/// the returned map; callers default those to `0`.
pub(super) async fn count_children_grouped(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    parent_ids: &[Uuid],
) -> Result<HashMap<Uuid, u64>, DomainError> {
    #[derive(FromQueryResult)]
    struct ChildCountRow {
        parent_id: Uuid,
        cnt: i64,
    }

    if parent_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let connection = repo.db.conn()?;

    let rows = tenants::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(tenants::Column::ParentId.is_in(parent_ids.iter().copied()))
                .add(tenants::Column::Status.ne(TenantStatus::Provisioning.as_smallint())),
        )
        .project_all(&connection, |q| {
            q.select_only()
                .column(tenants::Column::ParentId)
                .column_as(Expr::col(tenants::Column::Id).count(), "cnt")
                .group_by(tenants::Column::ParentId)
                .into_model::<ChildCountRow>()
        })
        .await
        .map_err(map_scope_err)?;

    Ok(rows
        .into_iter()
        .map(|r| (r.parent_id, u64::try_from(r.cnt).unwrap_or(0)))
        .collect())
}

pub(super) async fn count_tenants_by_status(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
) -> Result<Vec<(TenantStatus, bool, u64)>, DomainError> {
    let connection = repo.db.conn()?;
    // One cheap indexed COUNT per (status, self_managed) combo. All
    // eight combos are emitted — including zero-count ones — so the
    // `am_tenants` gauge keeps a stable series set across ticks.
    let statuses = [
        TenantStatus::Provisioning,
        TenantStatus::Active,
        TenantStatus::Suspended,
        TenantStatus::Deleted,
    ];
    let mut out = Vec::with_capacity(statuses.len() * 2);
    for status in statuses {
        for self_managed in [false, true] {
            let count = tenants::Entity::find()
                .secure()
                .scope_with(scope)
                .filter(
                    Condition::all()
                        .add(tenants::Column::Status.eq(status.as_smallint()))
                        .add(tenants::Column::SelfManaged.eq(self_managed)),
                )
                .count(&connection)
                .await
                .map_err(map_scope_err)?;
            out.push((status, self_managed, count));
        }
    }
    Ok(out)
}

pub(super) async fn count_closure_rows(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
) -> Result<u64, DomainError> {
    let connection = repo.db.conn()?;
    // `tenant_closure` carries no scoping dimensions (`#[secure(no_*)]`), so
    // `.scope_with` adds no predicate — this is a full table-size count.
    tenant_closure::Entity::find()
        .secure()
        .scope_with(scope)
        .count(&connection)
        .await
        .map_err(map_scope_err)
}

pub(super) async fn is_descendant(
    repo: &TenantRepoImpl,
    scope: &AccessScope,
    ancestor: Uuid,
    descendant: Uuid,
) -> Result<bool, DomainError> {
    // `is_descendant` answers a structural question — "does the
    // closure carry an `(ancestor, descendant)` row?" — that is
    // scope-independent by construction. `tenant_closure` is
    // `no_tenant/no_resource/no_owner/no_type`, so passing a
    // PDP-narrowed scope through `scope_with` would collapse to
    // `WHERE false` and silently return `false` for valid
    // ancestry edges. The PDP gate at the service layer is what
    // enforces caller scope; this read is the structural truth
    // that gate consults.
    let _ = scope;
    let conn = repo.db.conn()?;
    let count = tenant_closure::Entity::find()
        .secure()
        // Structural ancestry edge probe. `tenant_closure` is
        // `no_tenant/no_resource/no_owner/no_type` so the
        // `InTenantSubtree` predicate has no resolvable property to
        // clamp on; permanent `allow_all`. The PDP gate one layer up
        // is what enforces caller scope — this read is the structural
        // truth that gate consults.
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(tenant_closure::Column::AncestorId.eq(ancestor))
                .add(tenant_closure::Column::DescendantId.eq(descendant)),
        )
        .count(&conn)
        .await
        .map_err(map_scope_err)?;
    Ok(count > 0)
}

#[cfg(test)]
mod tenant_type_filter_tests {
    use super::*;
    use toolkit_gts::gts_id;

    // A valid `cf`-vendor chained tenant-type id (the one `service_tests`
    // uses). de0901 validates GTS string literals via `GtsOps::parse_id`; a
    // valid `cf` id is accepted, so no lint suppression is needed.
    const SAMPLE_TENANT_TYPE_GTS: &str =
        gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~");

    type Mapper = TenantODataMapper;
    type Field = TenantInfoFilterField;

    /// `$filter=tenant_type eq '<gts>'` maps the chained string to the same
    /// deterministic `UUIDv5` the storage column holds, so a value
    /// taken from the projection round-trips. Pins the derivation against
    /// `GtsId::to_uuid` so a future codec change is caught.
    #[test]
    fn tenant_type_string_maps_to_derived_uuidv5() {
        let expected = GtsId::try_new(SAMPLE_TENANT_TYPE_GTS)
            .expect("gts")
            .to_uuid();
        let out = <Mapper as FieldToColumn<Field>>::map_value(
            Field::TenantType,
            FilterOp::Eq,
            &ODataValue::String(SAMPLE_TENANT_TYPE_GTS.to_owned()),
        )
        .expect("derive ok");
        match out {
            ODataValue::Uuid(u) => assert_eq!(u, expected, "string must map to its UUIDv5"),
            other => panic!("expected ODataValue::Uuid, got {other:?}"),
        }
    }

    #[test]
    fn tenant_type_rejects_ordered_operator() {
        let err = <Mapper as FieldToColumn<Field>>::map_value(
            Field::TenantType,
            FilterOp::Lt,
            &ODataValue::String(SAMPLE_TENANT_TYPE_GTS.to_owned()),
        )
        .expect_err("ordered op must be rejected");
        assert!(err.contains("tenant_type"), "got {err}");
    }

    #[test]
    fn tenant_type_invalid_string_errors() {
        let err = <Mapper as FieldToColumn<Field>>::map_value(
            Field::TenantType,
            FilterOp::Eq,
            &ODataValue::String("not-a-gts-id".to_owned()),
        )
        .expect_err("invalid gts must error");
        assert!(err.contains("invalid `tenant_type`"), "got {err}");
    }

    #[test]
    fn tenant_type_is_not_orderable() {
        assert!(!<Mapper as FieldToColumn<Field>>::is_orderable(
            Field::TenantType
        ));
    }

    // Regression guard: the Tenants-page Status column sorts via
    // `$orderby=status`; the field must be orderable (lifecycle-ordinal
    // order — see `is_orderable` docs), not rejected with
    // `InvalidOrderByField` → 400 as it used to be.
    #[test]
    fn status_is_orderable() {
        assert!(<Mapper as FieldToColumn<Field>>::is_orderable(
            Field::Status
        ));
    }
}
