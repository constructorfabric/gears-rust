//! @cpt-dod:cpt-cf-bss-products-dod-derived-usage-type-store:p1
//! Derived usage type persistence (P-D-231): the type and its append-only versions, every read and
//! write scoped by the caller's `AccessScope` and the tenant, as the SKU repository is.
//!
//! A version is inserted once and never written again: the table's triggers refuse every `UPDATE`
//! and `DELETE`, and this module has no statement that tries. The digest a version carries is the
//! one its insert was given; no read here recomputes it.
use super::{SkuListError, driver_failure};
use crate::domain::derived::{
    DerivedUsageType, DerivedUsageTypeVersion, NewDerivedType, NewDerivedVersion,
};
use crate::infra::storage::{
    RepoError, RepoRefusal,
    entity::{derived_usage_type, derived_usage_type_version},
};
use sea_orm::sea_query::{Expr, ExprTrait, Query};
use sea_orm::{
    ColumnTrait, Condition, EntityTrait, FromQueryResult, JoinType, Order, QueryFilter,
    QuerySelect, RelationTrait, Set,
};
use std::collections::HashMap;
use time::OffsetDateTime;
use toolkit_db::odata::sea_orm_filter::{
    FieldToColumn, LimitCfg, ODataFieldMapping, PaginateOdataTryError, paginate_odata_try,
};
use toolkit_db::secure::{AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertExt};
use toolkit_odata::filter::{FieldKind, FilterField, FilterOp, ODataValue};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, Page, SortDir};
use uuid::Uuid;

/// The list's page: 50 by default, clamped at 200, as the SKU list (P-D-210).
pub const DERIVED_PAGE: LimitCfg = LimitCfg {
    default: 50,
    max: 200,
};

fn type_of(m: derived_usage_type::Model) -> DerivedUsageType {
    DerivedUsageType {
        tenant_id: m.tenant_id,
        id: m.id,
        code: m.code,
        name: m.name,
        created_by: m.created_by,
        created_at: m.created_at,
    }
}

fn version_of(m: derived_usage_type_version::Model) -> Result<DerivedUsageTypeVersion, RepoError> {
    let version = u32::try_from(m.version).map_err(|_| {
        RepoError::CorruptRow(format!(
            "derived usage type {} version {} is out of range",
            m.type_id, m.version
        ))
    })?;
    Ok(DerivedUsageTypeVersion {
        tenant_id: m.tenant_id,
        type_id: m.type_id,
        version,
        declaration_json: m.declaration_json,
        digest: m.digest,
        created_by: m.created_by,
        created_at: m.created_at,
    })
}

/// A unique violation of this module's two keys as its refusal; any other failure as the driver's.
fn refusal(context: &str, e: ScopeError) -> RepoError {
    if e.is_unique_violation() {
        let s = e.to_string();
        if s.contains("products_derived_usage_type_version") {
            return RepoError::Refused(RepoRefusal::DerivedVersionTaken);
        }
        if s.contains("uq_products_derived_usage_type_code")
            || s.contains("products_derived_usage_type.code")
        {
            return RepoError::Refused(RepoRefusal::DerivedCodeTaken);
        }
    }
    driver_failure(context.to_owned(), e)
}

/// Insert a type, its `id` fresh.
/// # Errors
/// `DERIVED_CODE_TAKEN` when the tenant has the code; scoped storage failures.
pub async fn create_type(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    new: NewDerivedType,
    created_by: Uuid,
    now: OffsetDateTime,
) -> Result<DerivedUsageType, RepoError> {
    let model = derived_usage_type::ActiveModel {
        tenant_id: Set(tenant_id),
        id: Set(Uuid::now_v7()),
        code: Set(new.code),
        name: Set(new.name),
        created_by: Set(created_by),
        created_at: Set(now),
    };
    derived_usage_type::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure("derived usage type scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map(type_of)
        .map_err(|e| refusal("insert derived usage type", e))
}

/// Append a version.
/// # Errors
/// `CONTENDED` (`DerivedVersionTaken`) when the number is taken; scoped storage failures.
pub async fn insert_version(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    new: NewDerivedVersion,
) -> Result<DerivedUsageTypeVersion, RepoError> {
    let model = derived_usage_type_version::ActiveModel {
        tenant_id: Set(tenant_id),
        type_id: Set(new.type_id),
        version: Set(i64::from(new.version)),
        declaration_json: Set(new.declaration_json),
        digest: Set(new.digest),
        created_by: Set(new.created_by),
        created_at: Set(new.created_at),
    };
    let row = derived_usage_type_version::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure("derived usage type version scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map_err(|e| refusal("insert derived usage type version", e))?;
    version_of(row)
}

/// The tenant's type with `code`, within the scope.
/// # Errors
/// Scoped storage failures.
pub async fn find_type(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    code: &str,
) -> Result<Option<DerivedUsageType>, RepoError> {
    derived_usage_type::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(derived_usage_type::Column::TenantId.eq(tenant_id))
                .add(derived_usage_type::Column::Code.eq(code)),
        )
        .one(runner)
        .await
        .map(|m| m.map(type_of))
        .map_err(|e| driver_failure("find derived usage type".into(), e))
}

fn version_key(tenant_id: Uuid, type_id: Uuid) -> Condition {
    Condition::all()
        .add(derived_usage_type_version::Column::TenantId.eq(tenant_id))
        .add(derived_usage_type_version::Column::TypeId.eq(type_id))
}

/// One version of the tenant's type, within the scope.
/// # Errors
/// Scoped storage failures; a stored version out of range is a corrupt row.
pub async fn find_version(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    type_id: Uuid,
    version: u32,
) -> Result<Option<DerivedUsageTypeVersion>, RepoError> {
    derived_usage_type_version::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            version_key(tenant_id, type_id)
                .add(derived_usage_type_version::Column::Version.eq(i64::from(version))),
        )
        .one(runner)
        .await
        .map_err(|e| driver_failure("find derived usage type version".into(), e))?
        .map(version_of)
        .transpose()
}

/// Every version of the tenant's type, oldest first.
/// # Errors
/// Scoped storage failures; a stored version out of range is a corrupt row.
pub async fn list_versions(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    type_id: Uuid,
) -> Result<Vec<DerivedUsageTypeVersion>, RepoError> {
    derived_usage_type_version::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(version_key(tenant_id, type_id))
        .order_by(derived_usage_type_version::Column::Version, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("derived usage type versions".into(), e))?
        .into_iter()
        .map(version_of)
        .collect()
}

/// `(type_id, max(version))` for the page, one grouped read. The outer query keeps the row whose
/// version is that maximum, so the caller gets the version, not only its number (P-D-257).
fn latest_version_numbers(tenant_id: Uuid, types: &[Uuid]) -> sea_orm::sea_query::SelectStatement {
    Query::select()
        .column(derived_usage_type_version::Column::TypeId)
        .expr(
            Expr::col((
                derived_usage_type_version::Entity,
                derived_usage_type_version::Column::Version,
            ))
            .max(),
        )
        .from(derived_usage_type_version::Entity)
        .and_where(derived_usage_type_version::Column::TenantId.eq(tenant_id))
        .and_where(derived_usage_type_version::Column::TypeId.is_in(types.iter().copied()))
        .group_by_col(derived_usage_type_version::Column::TypeId)
        .to_owned()
}

/// The latest version row of each of the tenant's `types`, in ONE grouped read; a type without a
/// version is absent. The caller's scope filters the returned rows. The grouped maximum repeats
/// the tenant and the page's type ids.
/// # Errors
/// Scoped storage failures; a stored version out of range is a corrupt row.
pub async fn latest_versions(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    types: &[Uuid],
) -> Result<HashMap<Uuid, DerivedUsageTypeVersion>, RepoError> {
    if types.is_empty() {
        return Ok(HashMap::new());
    }
    derived_usage_type_version::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(derived_usage_type_version::Column::TenantId.eq(tenant_id))
                .add(derived_usage_type_version::Column::TypeId.is_in(types.iter().copied()))
                .add(
                    Expr::tuple([
                        Expr::col((
                            derived_usage_type_version::Entity,
                            derived_usage_type_version::Column::TypeId,
                        )),
                        Expr::col((
                            derived_usage_type_version::Entity,
                            derived_usage_type_version::Column::Version,
                        )),
                    ])
                    .in_subquery(latest_version_numbers(tenant_id, types)),
                ),
        )
        .all(runner)
        .await
        .map_err(|e| driver_failure("latest derived usage type versions".into(), e))?
        .into_iter()
        .map(|row| {
            let version = version_of(row)?;
            Ok((version.type_id, version))
        })
        .collect()
}

/// The list's pager fields: `code`, with `id` as the tie-break. The list takes no `$filter` and no
/// `$orderby` (plan Out); the fields exist for the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DerivedListField {
    Code,
    Id,
}
impl FilterField for DerivedListField {
    const FIELDS: &'static [Self] = &[Self::Code, Self::Id];
    fn name(&self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Id => "id",
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::Code => FieldKind::String,
            Self::Id => FieldKind::Uuid,
        }
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}
/// How the pager reads the type row for each field.
pub struct DerivedListMapping;
impl FieldToColumn<DerivedListField> for DerivedListMapping {
    type Column = derived_usage_type::Column;
    fn map_field(field: DerivedListField) -> derived_usage_type::Column {
        match field {
            DerivedListField::Code => derived_usage_type::Column::Code,
            DerivedListField::Id => derived_usage_type::Column::Id,
        }
    }
    fn map_value(
        _field: DerivedListField,
        _op: FilterOp,
        value: &ODataValue,
    ) -> Result<ODataValue, String> {
        Ok(value.clone())
    }
}
impl ODataFieldMapping<DerivedListField> for DerivedListMapping {
    type Entity = derived_usage_type::Entity;
    fn extract_cursor_value(
        model: &derived_usage_type::Model,
        field: DerivedListField,
    ) -> sea_orm::Value {
        match field {
            DerivedListField::Code => sea_orm::Value::String(Some(model.code.clone())),
            DerivedListField::Id => sea_orm::Value::Uuid(Some(model.id)),
        }
    }
}

/// One page of the tenant's types, by code (tie-break id); `$top` defaults to 50 and is clamped
/// at 200, and a cursor continues the walk.
/// # Errors
/// [`SkuListError::Query`] for a cursor the pager refuses; [`SkuListError::Repo`] for storage.
pub async fn list(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    query: &ODataQuery,
) -> Result<Page<DerivedUsageType>, SkuListError> {
    let mut query = query.clone();
    if query.cursor.is_none() && query.order.0.is_empty() {
        query.order = ODataOrderBy(vec![OrderKey {
            field: DerivedListField::Code.name().to_owned(),
            dir: SortDir::Asc,
        }]);
    }
    let select = derived_usage_type::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(derived_usage_type::Column::TenantId.eq(tenant_id)));
    paginate_odata_try::<
        DerivedListField,
        DerivedListMapping,
        derived_usage_type::Entity,
        DerivedUsageType,
        _,
        RepoError,
        _,
    >(
        select,
        runner,
        &query,
        (DerivedListField::Id.name(), SortDir::Asc),
        DERIVED_PAGE,
        |m| Ok(type_of(m)),
    )
    .await
    .map_err(|e| match e {
        PaginateOdataTryError::OData(toolkit_odata::Error::Db(message)) => {
            SkuListError::Repo(RepoError::Driver {
                context: "list derived usage types".into(),
                source: sea_orm::DbErr::Custom(message),
            })
        }
        PaginateOdataTryError::OData(other) => SkuListError::Query(other),
        PaginateOdataTryError::MapError(e) => SkuListError::Repo(e),
    })
}

#[derive(Debug, FromQueryResult)]
struct MeterRow {
    code: String,
    version: i64,
    declaration_json: serde_json::Value,
}

/// The output unit of each referenced derived version, in ONE join (P-D-259). A meter that does
/// not parse, or that the tenant does not hold, is absent. The caller's scope filters the rows;
/// pass `tenant_only()` so a SKU resource scope does not hide the type.
///
/// PROBE-9-13-6: this is one read for the whole set, not one read per SKU.
///
/// # Errors
/// Scoped storage failures.
pub async fn output_units(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    meters: &[String],
) -> Result<HashMap<String, String>, RepoError> {
    let pairs: Vec<(String, i64)> = meters
        .iter()
        .filter_map(|meter| {
            let id = bss_products_sdk::derived::MeterId::parse(meter).ok()?;
            Some((id.code().to_owned(), i64::from(id.version())))
        })
        .collect();
    if pairs.is_empty() {
        return Ok(HashMap::new());
    }
    let codes: Vec<String> = pairs.iter().map(|(code, _)| code.clone()).collect();
    let versions: Vec<i64> = pairs.iter().map(|(_, version)| *version).collect();
    let wanted: std::collections::HashSet<(String, i64)> = pairs.into_iter().collect();
    let rows: Vec<MeterRow> = derived_usage_type_version::Entity::find()
        .filter(derived_usage_type_version::Column::TenantId.eq(tenant_id))
        .filter(derived_usage_type_version::Column::Version.is_in(versions))
        .filter(derived_usage_type::Column::Code.is_in(codes))
        .join(
            JoinType::InnerJoin,
            derived_usage_type_version::Relation::Type.def(),
        )
        .secure()
        .scope_with(scope)
        .project_all(runner, |query| {
            query
                .select_only()
                .column_as(derived_usage_type::Column::Code, "code")
                .column_as(derived_usage_type_version::Column::Version, "version")
                .column_as(
                    derived_usage_type_version::Column::DeclarationJson,
                    "declaration_json",
                )
                .into_model::<MeterRow>()
        })
        .await
        .map_err(|e| driver_failure("derived output units".into(), e))?;
    let mut units = HashMap::new();
    for row in rows {
        if !wanted.contains(&(row.code.clone(), row.version)) {
            continue;
        }
        let Some(unit) = row
            .declaration_json
            .get("output_unit")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Ok(version) = u32::try_from(row.version) else {
            continue;
        };
        if let Ok(meter) = bss_products_sdk::derived::MeterId::new(&row.code, version) {
            units.insert(meter.format(), unit.to_owned());
        }
    }
    Ok(units)
}

#[cfg(test)]
#[path = "derived_usage_type_repo_tests.rs"]
mod derived_usage_type_repo_tests;
