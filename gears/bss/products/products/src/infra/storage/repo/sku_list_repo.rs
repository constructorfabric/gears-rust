//! The SKU list on the toolkit's `OData` pager, and its tab counts (P-D-210, P-D-211).
//!
//! One field vocabulary, [`SkuListField`], serves the pager: every field a `$filter` may name and
//! every field an `$orderby` may name. The REST door publishes two narrower views of it
//! (`api::rest::sku_list`): the filter fields (no `updated_at`) and the order fields (`code`,
//! `name`, `updated_at`, and the tie-break `id`). The mapping below enforces both again, so a
//! caller that skipped the door's check still cannot filter by `updated_at` or order by a field
//! with no honest order.
//! @cpt-dod:cpt-cf-bss-products-dod-list-search:p1
use super::{driver_failure, sku_repo::sku_of};
use crate::infra::storage::{RepoError, entity::sku};
use bss_products_sdk::models::{Lifecycle, Sku, SkuType};
use bss_rest::archived::Archived;
use sea_orm::sea_query::{BinOper, Expr, ExprTrait, Func};
use sea_orm::{
    ColumnTrait, Condition, DbBackend, DbErr, EntityTrait, FromQueryResult, QuerySelect,
};
use toolkit_db::odata::sea_orm_filter::{
    FieldToColumn, LimitCfg, ODataFieldMapping, PaginateOdataTryError, escape_like,
    paginate_odata_try,
};
use toolkit_db::secure::{AccessScope, DBRunner, SecureEntityExt};
use toolkit_odata::filter::{FieldKind, FilterField, FilterOp, ODataValue};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, Page, SortDir};
use uuid::Uuid;

/// The page size when the caller names none, and the most a page holds (`$top` is clamped).
pub const SKU_PAGE: LimitCfg = LimitCfg {
    default: 50,
    max: 200,
};

/// Every field of the SKU list's pager (see the module doc for the published views).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkuListField {
    Id,
    Code,
    Name,
    Lifecycle,
    Type,
    CategoryId,
    PendingUnitId,
    RetirePending,
    /// The archive mark (P-D-263): taken out of the `$filter` before the pager
    /// ([`bss_rest::archived::take_archived`]), never compared as a column.
    Archived,
    UpdatedAt,
}
impl FilterField for SkuListField {
    const FIELDS: &'static [Self] = &[
        Self::Id,
        Self::Code,
        Self::Name,
        Self::Lifecycle,
        Self::Type,
        Self::CategoryId,
        Self::PendingUnitId,
        Self::RetirePending,
        Self::Archived,
        Self::UpdatedAt,
    ];
    fn name(&self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Code => "code",
            Self::Name => "name",
            Self::Lifecycle => "lifecycle",
            Self::Type => "type",
            Self::CategoryId => "category_id",
            Self::PendingUnitId => "pending_unit_id",
            Self::RetirePending => "retire_pending",
            Self::Archived => bss_rest::archived::ARCHIVED,
            Self::UpdatedAt => "updated_at",
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::Id | Self::CategoryId | Self::PendingUnitId => FieldKind::Uuid,
            Self::RetirePending | Self::Archived => FieldKind::Bool,
            Self::Code | Self::Name | Self::Lifecycle | Self::Type => FieldKind::String,
            Self::UpdatedAt => FieldKind::DateTimeUtc,
        }
    }
    /// Whether the column can hold no value: only these compare with `null` (`eq null`: no
    /// category; `ne null`: in review). The toolkit's parser refuses `null` on every other field.
    fn nullable(&self) -> bool {
        matches!(self, Self::CategoryId | Self::PendingUnitId)
    }
    /// Exact names only: the list has no property paths.
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}
impl SkuListField {
    /// Whether the field may key an order and a cursor: never a nullable field (the cursor's
    /// comparison has no answer for a null key) and never a filter-only one.
    #[must_use]
    pub const fn orderable(self) -> bool {
        matches!(self, Self::Id | Self::Code | Self::Name | Self::UpdatedAt)
    }
}
/// How the pager reads the SKU head for each field.
pub struct SkuListMapping;
impl FieldToColumn<SkuListField> for SkuListMapping {
    type Column = sku::Column;
    fn map_field(field: SkuListField) -> sku::Column {
        match field {
            SkuListField::Id => sku::Column::Id,
            SkuListField::Code => sku::Column::Code,
            SkuListField::Name => sku::Column::Name,
            SkuListField::Lifecycle => sku::Column::Lifecycle,
            SkuListField::Type => sku::Column::Type,
            SkuListField::CategoryId => sku::Column::CategoryId,
            SkuListField::PendingUnitId => sku::Column::PendingUnitId,
            SkuListField::RetirePending => sku::Column::RetirePending,
            SkuListField::Archived => sku::Column::ArchivedAt,
            SkuListField::UpdatedAt => sku::Column::UpdatedAt,
        }
    }
    /// `updated_at` orders only: on `SQLite` a `$filter` would bind chrono's `+00:00` against
    /// the stored RFC 3339 `Z`, and a text comparison lies at the boundary. `lifecycle` and
    /// `type` compare (`eq`, `ne`, `in`) with one of their closed values only. A text function
    /// on `type` still reaches here; one on `lifecycle` is served before the pager, as the
    /// `CASE`'s `in` over the tokens it matches (P-D-249, P-D-264). `null` reaches here only on
    /// a nullable field: the toolkit's parser refuses it on the others
    /// ([`FilterField::nullable`]).
    fn map_value(
        field: SkuListField,
        op: FilterOp,
        value: &ODataValue,
    ) -> Result<ODataValue, String> {
        if field == SkuListField::UpdatedAt {
            return Err("`updated_at` orders the list and is not a filter field".to_owned());
        }
        if field == SkuListField::Archived {
            // P-D-263: the list takes its `archived` terms out first; a term left here was not
            // one it can serve.
            return Err(bss_rest::archived::ARCHIVED_FILTER_REFUSED.to_owned());
        }
        let closed: Option<fn(&str) -> bool> = match field {
            SkuListField::Lifecycle => Some(|v| Lifecycle::parse(v).is_some()),
            SkuListField::Type => Some(|v| SkuType::parse(v).is_some()),
            _ => None,
        };
        if let Some(known) = closed
            && matches!(op, FilterOp::Eq | FilterOp::Ne | FilterOp::In)
        {
            match value {
                ODataValue::String(v) if known(v) => {}
                other => return Err(format!("unknown {}: {other}", field.name())),
            }
        }
        Ok(value.clone())
    }
    fn is_orderable(field: SkuListField) -> bool {
        field.orderable()
    }
}
impl ODataFieldMapping<SkuListField> for SkuListMapping {
    type Entity = sku::Entity;
    fn extract_cursor_value(model: &sku::Model, field: SkuListField) -> sea_orm::Value {
        match field {
            SkuListField::Id => sea_orm::Value::Uuid(Some(model.id)),
            SkuListField::Code => sea_orm::Value::String(Some(model.code.clone())),
            SkuListField::Name => sea_orm::Value::String(Some(model.name.clone())),
            SkuListField::Lifecycle => sea_orm::Value::String(Some(model.lifecycle.clone())),
            SkuListField::Type => sea_orm::Value::String(Some(model.r#type.clone())),
            SkuListField::CategoryId => sea_orm::Value::Uuid(model.category_id),
            SkuListField::PendingUnitId => sea_orm::Value::Uuid(model.pending_unit_id),
            SkuListField::RetirePending => sea_orm::Value::Bool(Some(model.retire_pending)),
            SkuListField::Archived => sea_orm::Value::Bool(Some(model.archived_at.is_some())),
            SkuListField::UpdatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(model.updated_at))
            }
        }
    }
}

/// What the list and the counts narrow the tenant's SKUs by, besides `$filter`.
#[derive(Debug, Clone, Default)]
pub struct SkuListFilter {
    /// `q`: a case-insensitive substring of the code, name, unit, usage type or GL code.
    pub text: Option<String>,
    /// `priced`: in or out of pricing's priced set (P-D-212).
    pub priced: Option<SetFilter>,
    /// `in_plan`: in or out of pricing's in-plan set (P-D-212).
    pub in_plan: Option<SetFilter>,
    /// `priced_in` (in) or `not_priced_in` (out): pricing's set of one book's SKUs (P-D-246).
    pub book: Option<SetFilter>,
    /// `not_in_revision` (out): pricing's set of one revision's SKUs (P-D-246).
    pub revision: Option<SetFilter>,
}
/// Keep the SKUs in `ids` (`member`), or the SKUs outside it.
#[derive(Debug, Clone)]
pub struct SetFilter {
    pub member: bool,
    pub ids: Vec<Uuid>,
}

/// `id` in `ids`, with ONE bind whatever the set's size: a JSON array read by `json_each` on
/// `SQLite` (a UUID is stored as 16 bytes there, hence `unhex`), a `uuid[]` on Postgres.
//
// Raw SQL, on purpose (whole-branch review RS-26): sea-query's `is_in` binds one parameter per id,
// so a set of pricing's usage (thousands of SKUs) would meet the dialects' bind limits and make
// the statement's text vary with its size, and sea-query spells neither a `uuid[]` cast nor a
// `json_each` subselect. The ids stay bound, and the select around this condition stays
// `.secure().scope_with(scope)`, so the tenant scope is untouched.
// Upstream gears use the same pattern in repository code: account-management
// `infra/lease/manager.rs` (`Expr::cust("NOW()")`, `INTERVAL`) and
// `infra/storage/repo_impl/retention.rs` (`make_interval`, `julianday`), and settings-service
// `infra/storage/search_repo.rs` (`LIKE … ESCAPE`, the JSON null checks). A toolkit-db helper
// would be a change to a foreign crate, proposed upstream on its own (owner, O3/O4).
pub(super) fn membership(backend: DbBackend, ids: &[Uuid]) -> Condition {
    let expr = if backend == DbBackend::Postgres {
        let array = format!(
            "{{{}}}",
            ids.iter()
                .map(|id| id.hyphenated().to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        Expr::cust_with_values(r#""products_sku"."id" = ANY(CAST($1 AS uuid[]))"#, [array])
    } else {
        let hex: Vec<String> = ids
            .iter()
            .map(|id| id.simple().to_string().to_uppercase())
            .collect();
        let json = serde_json::Value::from(hex).to_string();
        Expr::cust_with_values(
            r#""products_sku"."id" IN (SELECT unhex("value") FROM json_each(?))"#,
            [json],
        )
    };
    Condition::all().add(expr)
}
fn set_condition(backend: DbBackend, set: &SetFilter) -> Condition {
    let inside = membership(backend, &set.ids);
    if set.member { inside } else { inside.not() }
}

/// The collation `q` folds case through on Postgres: ICU's root locale, which folds Unicode
/// whatever the database's own locale is (a `C` database's `lower()` folds ASCII only). A
/// deployment's Postgres must be built with ICU (P-D-210).
pub const PG_FOLD_COLLATION: &str = "und-x-icu";

/// `lower(expr)`, folded through [`PG_FOLD_COLLATION`] on Postgres and through the database's own
/// `lower()` (ASCII only) on `SQLite`.
//
// Raw SQL, on purpose (whole-branch review RS-26): sea-query has no `COLLATE` on an expression,
// and the fold must name the ICU collation, or a `C`-locale database folds ASCII only (P-D-210).
// The folded text stays a bound value.
// Upstream gears use the same pattern in repository code: account-management
// `infra/lease/manager.rs` (`Expr::cust("NOW()")`, `INTERVAL`) and
// `infra/storage/repo_impl/retention.rs` (`make_interval`, `julianday`), and settings-service
// `infra/storage/search_repo.rs` (`LIKE … ESCAPE`, the JSON null checks). A toolkit-db helper
// would be a change to a foreign crate, proposed upstream on its own (owner, O3/O4).
fn folded(backend: DbBackend, expr: Expr) -> Expr {
    if backend == DbBackend::Postgres {
        Expr::cust_with_expr(format!(r#"lower($1 COLLATE "{PG_FOLD_COLLATION}")"#), expr)
    } else {
        Expr::expr(Func::lower(expr))
    }
}

/// `q` over the five text columns: `lower(column) LIKE lower(pattern) ESCAPE '\'`, the caller's
/// text matched literally (`%`, `_` and `\` escaped). Both sides fold case the same way: through
/// the ICU root collation on Postgres, so Unicode case folds whatever the database's locale, and
/// through `SQLite`'s `lower()`, ASCII only (P-D-210). An unset column never matches.
fn text_condition(text: &str, backend: DbBackend) -> Condition {
    let pattern = format!("%{}%", escape_like(text));
    [
        sku::Column::Code,
        sku::Column::Name,
        sku::Column::Unit,
        sku::Column::UsageTypeRef,
        sku::Column::GlCode,
    ]
    .into_iter()
    .fold(Condition::any(), |any, column| {
        // `LikeExpr` binds its pattern as it is; the pattern here goes through `lower()` too,
        // so the `LIKE … ESCAPE` is spelled as the nested binary `LikeExpr` itself builds.
        let lowered = folded(backend, Expr::val(pattern.clone())).binary(
            BinOper::Escape,
            Expr::Constant(sea_orm::Value::Char(Some('\\'))),
        );
        any.add(folded(backend, Expr::col((sku::Entity, column))).binary(BinOper::Like, lowered))
    })
}

/// The tenant's SKUs narrowed by `filter`, before `$filter`, the cursor and the order.
#[must_use]
pub fn list_condition(tenant: Uuid, filter: &SkuListFilter, backend: DbBackend) -> Condition {
    let mut c = Condition::all().add(sku::Column::TenantId.eq(tenant));
    if let Some(text) = filter.text.as_deref() {
        c = c.add(text_condition(text, backend));
    }
    for set in [
        &filter.priced,
        &filter.in_plan,
        &filter.book,
        &filter.revision,
    ]
    .into_iter()
    .flatten()
    {
        c = c.add(set_condition(backend, set));
    }
    c
}

/// The archive-mark condition a list applies (P-D-263): the rows without a mark by default, only
/// the marked ones for `archived eq true`, and none for two terms that disagree.
pub(crate) fn archive_mark<C: ColumnTrait>(column: C, kept: Archived) -> Condition {
    match kept {
        Archived::Hidden => Condition::all().add(column.is_null()),
        Archived::Only => Condition::all().add(column.is_not_null()),
        Archived::Neither => Condition::all()
            .add(column.is_null())
            .add(column.is_not_null()),
    }
}

/// The `$filter` without its top-level `archived` terms, and the rows they keep (P-D-263). A term
/// the list cannot serve is `InvalidFilter`, a 400.
/// # Errors
/// [`bss_rest::archived::ARCHIVED_FILTER_REFUSED`] as `toolkit_odata::Error::InvalidFilter`.
pub fn take_archived(
    filter: Option<toolkit_odata::ast::Expr>,
) -> Result<(Option<toolkit_odata::ast::Expr>, Archived), toolkit_odata::Error> {
    bss_rest::archived::take_archived_opt(filter).map_err(toolkit_odata::Error::InvalidFilter)
}

/// A list read refused or failed.
#[derive(Debug)]
pub enum SkuListError {
    /// The query itself: a filter value, an order field, a cursor (400).
    Query(toolkit_odata::Error),
    /// Storage; a driver failure keeps its message for the retry classifier.
    Repo(RepoError),
}

/// A lifecycle term the `CASE` does not serve. The list and the counts both answer 400 with this
/// text (P-D-249, P-D-264).
pub(crate) const LIFECYCLE_FILTER_REFUSED: &str = "a `lifecycle` term must be `eq`, `ne`, `in`, `contains`, `startswith` or \
     `endswith`, joined only by top-level `and`; a `lifecycle` term under `or` or `not` is refused";

/// The closed set a text function on `lifecycle` is matched against (P-D-264).
const LIFECYCLES: [Lifecycle; 4] = [
    Lifecycle::Draft,
    Lifecycle::Published,
    Lifecycle::Deprecated,
    Lifecycle::Retired,
];

fn is_lifecycle(expr: &toolkit_odata::ast::Expr) -> bool {
    matches!(expr, toolkit_odata::ast::Expr::Identifier(name) if name == SkuListField::Lifecycle.name())
}
fn names_lifecycle(expr: &toolkit_odata::ast::Expr) -> bool {
    use toolkit_odata::ast::Expr;
    match expr {
        Expr::Identifier(name) => name == SkuListField::Lifecycle.name(),
        Expr::Value(_) => false,
        Expr::And(a, b) | Expr::Or(a, b) | Expr::Compare(a, _, b) => {
            names_lifecycle(a) || names_lifecycle(b)
        }
        Expr::Not(inner) => names_lifecycle(inner),
        Expr::In(inner, list) => names_lifecycle(inner) || list.iter().any(names_lifecycle),
        Expr::Function(_, args) => args.iter().any(names_lifecycle),
    }
}
fn lifecycle_token(expr: &toolkit_odata::ast::Expr) -> Result<String, String> {
    match expr {
        toolkit_odata::ast::Expr::Value(toolkit_odata::ast::Value::String(token))
            if Lifecycle::parse(token).is_some() =>
        {
            Ok(token.clone())
        }
        toolkit_odata::ast::Expr::Value(toolkit_odata::ast::Value::String(token)) => {
            Err(format!("unknown lifecycle: {token}"))
        }
        _ => Err("unknown lifecycle".into()),
    }
}
/// The lifecycle tokens that `contains`, `startswith` or `endswith` on `lifecycle` matches
/// (P-D-264). The text is matched case-sensitively, as the tokens are lower-case; the function
/// name regardless of case, as the toolkit's filter conversion reads it. `None` for any other
/// function, or other arguments than `lifecycle` and a text.
fn lifecycle_text_tokens(
    name: &str,
    args: &[toolkit_odata::ast::Expr],
) -> Option<Vec<&'static str>> {
    use toolkit_odata::ast::{Expr, Value};
    let [field, Expr::Value(Value::String(text))] = args else {
        return None;
    };
    if !is_lifecycle(field) {
        return None;
    }
    let matches: fn(&str, &str) -> bool = match name.to_ascii_lowercase().as_str() {
        "contains" => |token: &str, text: &str| token.contains(text),
        "startswith" => |token: &str, text: &str| token.starts_with(text),
        "endswith" => |token: &str, text: &str| token.ends_with(text),
        _ => return None,
    };
    Some(
        LIFECYCLES
            .iter()
            .map(|lifecycle| lifecycle.as_str())
            .filter(|token| matches(token, text))
            .collect(),
    )
}
/// A term that is not pulled out: kept for the pager, or [`LIFECYCLE_FILTER_REFUSED`] when it
/// names `lifecycle`.
fn kept_unless_lifecycle(
    expr: toolkit_odata::ast::Expr,
) -> Result<(Option<toolkit_odata::ast::Expr>, Condition), String> {
    if names_lifecycle(&expr) {
        return Err(LIFECYCLE_FILTER_REFUSED.to_owned());
    }
    Ok((Some(expr), Condition::all()))
}
/// Pull top-level `lifecycle` terms out of `expr` so they compare the effective lifecycle: `eq`,
/// `ne`, `in`, and a text function as the `in` of the tokens it matches (P-D-264). A lifecycle
/// term that is not pulled out — under `or` or `not` — is [`LIFECYCLE_FILTER_REFUSED`]. The
/// stored column is never the comparison.
pub(crate) fn take_lifecycle(
    expr: toolkit_odata::ast::Expr,
) -> Result<(Option<toolkit_odata::ast::Expr>, Condition), String> {
    use toolkit_odata::ast::{CompareOperator, Expr};
    match expr {
        Expr::And(left, right) => {
            let (left, left_cond) = take_lifecycle(*left)?;
            let (right, right_cond) = take_lifecycle(*right)?;
            let rest = match (left, right) {
                (Some(left), Some(right)) => Some(Expr::And(Box::new(left), Box::new(right))),
                (Some(one), None) | (None, Some(one)) => Some(one),
                (None, None) => None,
            };
            // Both sides inside one `all`: a side's condition may be the `CASE`'s `any` (or its
            // negation), and adding to that would join the other side by `or`.
            Ok((rest, Condition::all().add(left_cond).add(right_cond)))
        }
        Expr::Compare(left, op, right) if is_lifecycle(&left) => {
            let token = lifecycle_token(&right)?;
            let today = crate::infra::storage::stored_now().date();
            let cond = match op {
                CompareOperator::Eq => super::sku_repo::effective_lifecycle_in(today, &[token]),
                CompareOperator::Ne => super::sku_repo::effective_lifecycle_ne(today, &token),
                _ => {
                    return Err("lifecycle compares with eq, ne or in".to_owned());
                }
            };
            Ok((None, cond))
        }
        Expr::In(left, values) if is_lifecycle(&left) => {
            let mut tokens = Vec::new();
            for value in values {
                tokens.push(lifecycle_token(&value)?);
            }
            Ok((
                None,
                super::sku_repo::effective_lifecycle_in(
                    crate::infra::storage::stored_now().date(),
                    &tokens,
                ),
            ))
        }
        Expr::Function(name, args) => match lifecycle_text_tokens(&name, &args) {
            // No token matched: an empty `any`, which sea-query renders as `FALSE`. The read
            // keeps nothing; it is not refused.
            Some(tokens) => Ok((
                None,
                super::sku_repo::effective_lifecycle_in(
                    crate::infra::storage::stored_now().date(),
                    &tokens,
                ),
            )),
            None => kept_unless_lifecycle(Expr::Function(name, args)),
        },
        other => kept_unless_lifecycle(other),
    }
}
/// One page of the tenant's SKUs: `filter`'s narrowing, then the query's `$filter`, cursor and
/// order (`code` when it names none), tie-broken by `id`; `$top` defaults to 50 and is clamped
/// at 200.
/// # Errors
/// [`SkuListError::Query`] for a value, order field or cursor the pager refuses;
/// [`SkuListError::Repo`] for storage and corrupt rows.
pub async fn page_skus(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    backend: DbBackend,
    filter: &SkuListFilter,
    query: &ODataQuery,
) -> Result<Page<Sku>, SkuListError> {
    let mut query = query.clone();
    // P-D-263: the archive mark is a nullable column the pager cannot compare as a boolean. The
    // top-level `archived` terms go first, and an archived SKU is hidden unless they ask for it.
    let (rest, archived) =
        take_archived(query.filter.take().map(|f| *f)).map_err(SkuListError::Query)?;
    query.filter = rest.map(Box::new);
    // The toolkit maps a field to a column, so it cannot express the effective-lifecycle CASE.
    // Top-level `lifecycle` terms (`eq`, `ne`, `in`, a text function, and those joined by `and`)
    // are applied as that CASE before the pager sees the rest (P-D-249, P-D-264). Any other
    // lifecycle term is refused, so the pager never compares the stored column.
    let lifecycle_filter = match query.filter.take() {
        Some(filter) => {
            let (rest, cond) = take_lifecycle(*filter).map_err(|message| {
                SkuListError::Query(toolkit_odata::Error::InvalidFilter(message))
            })?;
            query.filter = rest.map(Box::new);
            cond
        }
        None => Condition::all(),
    };
    if query.cursor.is_none() && query.order.0.is_empty() {
        query.order = ODataOrderBy(vec![OrderKey {
            field: SkuListField::Code.name().to_owned(),
            dir: SortDir::Asc,
        }]);
    }
    let select = sku::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(list_condition(tenant, filter, backend))
        .filter(archive_mark(sku::Column::ArchivedAt, archived))
        .filter(lifecycle_filter);
    let mut page =
        paginate_odata_try::<SkuListField, SkuListMapping, sku::Entity, Sku, _, RepoError, _>(
            select,
            runner,
            &query,
            (SkuListField::Id.name(), SortDir::Asc),
            SKU_PAGE,
            sku_of,
        )
        .await
        .map_err(|e| match e {
            // The pager renders the driver's error as text; kept as a driver failure so the
            // door's retry still sees a serialization failure or a busy database by its message.
            PaginateOdataTryError::OData(toolkit_odata::Error::Db(message)) => {
                SkuListError::Repo(RepoError::Driver {
                    context: "list SKUs".into(),
                    source: DbErr::Custom(message),
                })
            }
            PaginateOdataTryError::OData(other) => SkuListError::Query(other),
            PaginateOdataTryError::MapError(e) => SkuListError::Repo(e),
        })?;
    super::sku_repo::fill_served_units(runner, scope, tenant, &mut page.items)
        .await
        .map_err(SkuListError::Repo)?;
    Ok(page)
}

/// The tab counts of the list: every SKU that is not archived, those in each lifecycle, those in
/// review, and the archived ones (P-D-263).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SkuCounts {
    pub all: u64,
    pub draft: u64,
    pub published: u64,
    pub deprecated: u64,
    pub retired: u64,
    /// SKUs a pending approval unit locks (`pending_unit_id` set), in any lifecycle.
    pub in_review: u64,
    /// The archived SKUs (P-D-263); no other number counts them.
    pub archived: u64,
}
#[derive(Debug, FromQueryResult)]
struct LifecycleCount {
    lifecycle: String,
    n: i64,
    archived: i64,
    in_review: i64,
}
fn count(v: i64) -> Result<u64, RepoError> {
    u64::try_from(v).map_err(|_| RepoError::CorruptRow(format!("a negative count {v}")))
}

/// The counts of the tenant's SKUs narrowed by `filter` and by `condition` (the door's
/// `$filter` without its `lifecycle` and `archived` terms), in ONE grouped statement. An archived
/// SKU counts in `archived` only (P-D-263).
/// # Errors
/// Storage failures; a stored lifecycle outside the five is a corrupt row.
pub async fn count_skus(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    backend: DbBackend,
    filter: &SkuListFilter,
    condition: Option<Condition>,
) -> Result<SkuCounts, RepoError> {
    let mut c = list_condition(tenant, filter, backend);
    if let Some(condition) = condition {
        c = c.add(condition);
    }
    let rows = sku::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(c)
        .project_all(runner, |q| {
            let effective = super::sku_repo::effective_lifecycle_expr(
                crate::infra::storage::stored_now().date(),
            );
            q.select_only()
                .expr_as(effective, "lifecycle")
                .column_as(Expr::col((sku::Entity, sku::Column::Id)).count(), "n")
                // P-D-263: the archived SKUs of the group, and its SKUs in review that are not.
                .column_as(
                    Expr::col((sku::Entity, sku::Column::ArchivedAt)).count(),
                    "archived",
                )
                .column_as(
                    Expr::expr(Func::count(Expr::case(
                        Expr::col((sku::Entity, sku::Column::ArchivedAt)).is_null(),
                        Expr::col((sku::Entity, sku::Column::PendingUnitId)),
                    ))),
                    "in_review",
                )
                // By position: the effective lifecycle binds its day as a parameter, and Postgres
                // reads `CASE … $1` in the select and `CASE … $2` in a repeated GROUP BY as two
                // different expressions (42803). SQLite never notices, so only the pg tier proves it.
                .group_by(Expr::cust("1"))
                .into_model::<LifecycleCount>()
        })
        .await
        .map_err(|e| driver_failure("count SKUs".into(), e))?;
    let mut counts = SkuCounts::default();
    for row in rows {
        let archived = count(row.archived)?;
        let n = count(row.n)?.checked_sub(archived).ok_or_else(|| {
            RepoError::CorruptRow(format!("{archived} archived of {} SKUs", row.n))
        })?;
        counts.archived += archived;
        let slot = match Lifecycle::parse(&row.lifecycle) {
            Some(Lifecycle::Draft) => &mut counts.draft,
            Some(Lifecycle::Published) => &mut counts.published,
            Some(Lifecycle::Deprecated) => &mut counts.deprecated,
            Some(Lifecycle::Retired) => &mut counts.retired,
            None => {
                return Err(RepoError::CorruptRow(format!(
                    "SKU lifecycle {}",
                    row.lifecycle
                )));
            }
        };
        *slot += n;
        counts.all += n;
        counts.in_review += count(row.in_review)?;
    }
    Ok(counts)
}

#[cfg(test)]
#[path = "sku_list_repo_tests.rs"]
mod sku_list_repo_tests;
