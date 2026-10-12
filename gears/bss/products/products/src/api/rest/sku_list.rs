//! `GET /skus` on the toolkit's `OData` pager, and its tab counts `GET /skus/counts` (P-D-210,
//! P-D-211).
//!
//! The list takes `$filter` over [`SkuFilterField`], `$orderby` over [`SkuOrderField`] (tie-break
//! `id`), `$top` (alias `limit`; default 50, clamped at 200) and `cursor` (alias `$skiptoken`),
//! plus `q`, the usage filters `priced` and `in_plan` (P-D-212) and the picker keys `priced_in`,
//! `not_priced_in` and `not_in_revision` (P-D-246; each picker key is one call of the port's
//! `sku_ids_in`, bound as one value). Any other key is 400; `$select` and `$count` are refused.
//! The cursor carries a hash of `$filter`, `q`, the usage filters and the picker keys, so a cursor
//! replayed with other values is 400. Each item carries pricing's `usage`
//! from one port call per page, as before (P-D-197); a usage filter adds one call of the port's
//! `usage_sets`, and a filter it cannot answer fails the read (403 or 503), never widening it.
//!
//! The counts take the list's narrowing — `q`, the usage filters and `$filter` with its top-level
//! `lifecycle` comparisons (`eq`, `ne`, `in`, and those joined by `and`) dropped — and nothing
//! that pages or orders. A lifecycle term under `or` or `not`, or a text function on `lifecycle`,
//! is 400 on the list and on the counts.
//! @cpt-dod:cpt-cf-bss-products-dod-list-search:p1
use super::{
    ApiState, TxError, authz_error_to_canonical, category_tx_config, contention_db_err,
    dto::{ProductsSkuCounts, SkuListItem},
    require_authenticated, tx_to_canonical, usage,
};
use crate::{
    authz::{access_scope, actions, resource_types},
    domain::canonical::{canonical_rendering, content_digest},
    infra::storage::repo::{
        self, SetFilter, SkuListError, SkuListField, SkuListFilter, SkuListMapping,
    },
};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{
    Extension, Router,
    extract::{Query, rejection::QueryRejection},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use bss_products_sdk::sku_usage::{SkuUsageSets, UsageScope};
use bss_rest::conditional_get::{PRIVATE_REVALIDATE, respond};
use std::sync::Arc;
use toolkit::api::{
    OpenApiRegistry,
    canonical_prelude::{CanonicalError, resource_error},
    odata::OData,
    operation_builder::{OperationBuilder, OperationBuilderODataExt},
};
use toolkit_db::odata::filter_node_to_condition;
use toolkit_db::secure::AccessScope;
use toolkit_odata::{
    Error as ODataError, ODataQuery, Page,
    ast::Expr,
    errors::OdataError,
    filter::{FieldKind, FilterField, convert_expr_to_filter_node},
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

const SKUS: &str = "/bss-products/v1/skus";
const TAG: &str = "SKUs";
#[resource_error(gts_id!("cf.bss.products.sku.v1~"))]
struct SkuResource;

/// The reason every refused query key carries: the toolkit extractor's own for a `$` option.
pub(super) const UNSUPPORTED: &str = "UNSUPPORTED_QUERY_PARAM";

/// The query string as its raw pairs, in wire order (so every key is seen, repeats included).
pub(super) type RawQuery = Result<Query<Vec<(String, String)>>, QueryRejection>;

/// The fields a list or count `$filter` names: the pager's fields without `updated_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkuFilterField {
    Id,
    Code,
    Name,
    Lifecycle,
    Type,
    CategoryId,
    PendingUnitId,
    RetirePending,
    /// The archive mark (P-D-263): `archived eq true` lists only the archived SKUs.
    Archived,
}
impl SkuFilterField {
    const fn field(self) -> SkuListField {
        match self {
            Self::Id => SkuListField::Id,
            Self::Code => SkuListField::Code,
            Self::Name => SkuListField::Name,
            Self::Lifecycle => SkuListField::Lifecycle,
            Self::Type => SkuListField::Type,
            Self::CategoryId => SkuListField::CategoryId,
            Self::PendingUnitId => SkuListField::PendingUnitId,
            Self::RetirePending => SkuListField::RetirePending,
            Self::Archived => SkuListField::Archived,
        }
    }
}
impl FilterField for SkuFilterField {
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
    ];
    fn name(&self) -> &'static str {
        self.field().name()
    }
    fn kind(&self) -> FieldKind {
        self.field().kind()
    }
    /// `category_id` and `pending_unit_id` (P-D-210).
    fn nullable(&self) -> bool {
        self.field().nullable()
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}
/// The fields a list `$orderby` names: never a nullable or a filter-only field (ledger's
/// `ExceptionOrderField` pattern). `id` is the tie-break every order ends with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkuOrderField {
    Code,
    Name,
    UpdatedAt,
    Id,
}
impl SkuOrderField {
    const fn field(self) -> SkuListField {
        match self {
            Self::Code => SkuListField::Code,
            Self::Name => SkuListField::Name,
            Self::UpdatedAt => SkuListField::UpdatedAt,
            Self::Id => SkuListField::Id,
        }
    }
}
impl FilterField for SkuOrderField {
    const FIELDS: &'static [Self] = &[Self::Code, Self::Name, Self::UpdatedAt, Self::Id];
    fn name(&self) -> &'static str {
        self.field().name()
    }
    fn kind(&self) -> FieldKind {
        self.field().kind()
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}

/// Register the list and the counts; both read under `sku × read`.
pub(crate) fn register(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::get(SKUS)
        .operation_id("bss_products.list_skus")
        .summary("List, filter and search SKUs")
        .description(
            "One page of the tenant's SKUs (P-D-210). OData `$filter` over id, code, name, \
             lifecycle, retire_pending, archived, type, category_id (`eq null`: no category) and \
             pending_unit_id (`ne null`: in review). An archived SKU is left out unless the \
             filter asks `archived eq true`, which lists only the archived ones; `archived eq \
             false` is the default made explicit (P-D-263). `archived` compares with `eq` or `ne` \
             and a boolean, joined only by top-level `and`; any other use of it is 400. \
             `lifecycle` compares the effective lifecycle with `eq`, `ne` or `in`, or with \
             `contains`, `startswith` or `endswith` as the `in` of the lifecycle tokens the text \
             matches, case-sensitively (none matching keeps nothing; P-D-264), at the top level \
             or joined by `and`; a `lifecycle` term under `or` or `not` is 400. \
             `$orderby` over code, name, updated_at (tie-break id; default \
             code); `$top` (alias `limit`; default 50, clamped at 200) and `cursor` (alias \
             `$skiptoken`) from `page_info`. `q` is a case-insensitive substring of the code, \
             name, unit, usage type or GL code, matched literally (ASCII case folding on SQLite). \
             `priced` and `in_plan` (true or false) keep the SKUs pricing prices or a plan \
             names, or the others (P-D-212): 403 USAGE_FORBIDDEN when pricing refuses the \
             caller, 503 USAGE_UNAVAILABLE when it cannot answer. Any other key, `$select` and \
             `$count` are 400; a cursor replayed with another `$filter`, `q`, `priced` or \
             `in_plan` is 400. Each item carries pricing's `usage`, or null (P-D-197). The \
             pickers (P-D-246): `priced_in` or `not_priced_in`, a price book id (at most one of \
             the two), keeps the SKUs with an entry in that book, in any reference state, or the \
             others; `not_in_revision`, a plan revision id, keeps the SKUs its items do not name. \
             Each key is one call to pricing, under pricing price_book_entry read, and the \
             revision's also under plan read: 403 USAGE_FORBIDDEN when pricing refuses the \
             caller, 503 USAGE_UNAVAILABLE when it cannot answer. A book or a revision the tenant \
             does not hold is an empty set. The cursor carries the picker keys too. The multi-id \
             read is `$filter=id in (...)`, one page of at most `$top` 200, within the 8 KiB \
             filter. A matching If-None-Match is 304 with an empty body; the 200 carries a weak \
             ETag of its JSON and Cache-Control private, no-cache (P-D-261).",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .query_param_typed(
            "limit",
            false,
            "Page size, alias of $top (default 50, clamped at 200)",
            "integer",
        )
        .query_param_typed(
            "cursor",
            false,
            "Continuation from page_info (alias $skiptoken)",
            "string",
        )
        .query_param_typed(
            "q",
            false,
            "Case-insensitive substring of the code, name, unit, usage type or GL code",
            "string",
        )
        .query_param_typed(
            "priced",
            false,
            "true: SKUs pricing has an entry for; false: the others",
            "boolean",
        )
        .query_param_typed(
            "in_plan",
            false,
            "true: SKUs a live plan names through an entry; false: the others",
            "boolean",
        )
        .query_param_typed(
            "priced_in",
            false,
            "A price book id: only the SKUs with an entry in it (P-D-246)",
            "string",
        )
        .query_param_typed(
            "not_priced_in",
            false,
            "A price book id: only the SKUs without an entry in it (P-D-246)",
            "string",
        )
        .query_param_typed(
            "not_in_revision",
            false,
            "A plan revision id: only the SKUs its items do not name (P-D-246)",
            "string",
        )
        .param(super::preconditions::if_none_match_param())
        .handler(list_skus)
        .with_odata_filter::<SkuFilterField>()
        .with_odata_orderby::<SkuOrderField>()
        .json_response_with_schema::<Page<SkuListItem>>(
            openapi,
            StatusCode::OK,
            "One page of SKUs.",
        )
        .response_header(super::preconditions::weak_etag_header())
        .response_header(super::preconditions::revalidate_header())
        .no_content_response(
            StatusCode::NOT_MODIFIED,
            "The If-None-Match tag matches this body",
        )
        .response_header(super::preconditions::weak_etag_header())
        .response_header(super::preconditions::revalidate_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    OperationBuilder::get(format!("{SKUS}/counts"))
        .operation_id("bss_products.count_skus")
        .summary("Count SKUs by lifecycle and in review")
        .description(
            "The list's tab counts (P-D-211): every SKU, each lifecycle, and those in review \
             (`pending_unit_id` set), none of them archived, and the archived SKUs in `archived` \
             (P-D-263), narrowed like the list by `q`, `priced`, `in_plan` and `$filter`. \
             Top-level `lifecycle` terms (`eq`, `ne`, `in`, `contains`, `startswith` or \
             `endswith`, and those joined by `and`) and top-level `archived` comparisons are \
             dropped (P-D-264). A `lifecycle` term under `or` or `not` is 400. `$orderby`, `$top`/`limit`, \
             `cursor`/`$skiptoken` and `$select` are 400. The picker keys `priced_in`, `not_priced_in` (at most one of the \
             two) and `not_in_revision` narrow the counts as they narrow the list (P-D-246): 403 \
             USAGE_FORBIDDEN when pricing refuses the caller (a revision takes plan read beside \
             price_book_entry read), 503 USAGE_UNAVAILABLE when it cannot answer. A matching \
             If-None-Match is 304 with an empty body; the 200 carries a weak ETag of its JSON and \
             Cache-Control private, no-cache (P-D-261).",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .query_param_typed(
            "q",
            false,
            "Case-insensitive substring of the code, name, unit, usage type or GL code",
            "string",
        )
        .query_param_typed(
            "priced",
            false,
            "true: SKUs pricing has an entry for; false: the others",
            "boolean",
        )
        .query_param_typed(
            "in_plan",
            false,
            "true: SKUs a live plan names through an entry; false: the others",
            "boolean",
        )
        .query_param_typed(
            "priced_in",
            false,
            "A price book id: only the SKUs with an entry in it (P-D-246)",
            "string",
        )
        .query_param_typed(
            "not_priced_in",
            false,
            "A price book id: only the SKUs without an entry in it (P-D-246)",
            "string",
        )
        .query_param_typed(
            "not_in_revision",
            false,
            "A plan revision id: only the SKUs its items do not name (P-D-246)",
            "string",
        )
        .param(super::preconditions::if_none_match_param())
        .handler(count_skus)
        .with_odata_filter::<SkuFilterField>()
        .json_response_with_schema::<ProductsSkuCounts>(openapi, StatusCode::OK, "The SKU counts.")
        .response_header(super::preconditions::weak_etag_header())
        .response_header(super::preconditions::revalidate_header())
        .no_content_response(
            StatusCode::NOT_MODIFIED,
            "The If-None-Match tag matches this body",
        )
        .response_header(super::preconditions::weak_etag_header())
        .response_header(super::preconditions::revalidate_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// Read under `sku × read`, as every SKU read.
async fn read_scope(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
) -> Result<AccessScope, CanonicalError> {
    access_scope(enforcer, ctx, &resource_types::SKU, actions::READ, None)
        .await
        .map_err(|e| {
            authz_error_to_canonical(e, |reason| {
                SkuResource::permission_denied()
                    .with_reason(reason)
                    .create()
            })
        })
}

/// A 400 naming each offending key.
pub(super) fn refused(keys: &[(&str, String, &'static str)]) -> Result<(), CanonicalError> {
    let mut keys = keys.iter();
    let Some((key, detail, reason)) = keys.next() else {
        return Ok(());
    };
    let mut error = OdataError::invalid_argument().with_field_violation(*key, detail, *reason);
    for (key, detail, reason) in keys {
        error = error.with_field_violation(*key, detail, *reason);
    }
    Err(error.create())
}

/// What the list and the counts read besides the `OData` options.
#[derive(Debug, Clone, Default)]
pub(crate) struct ListParams {
    pub q: Option<String>,
    pub priced: Option<bool>,
    pub in_plan: Option<bool>,
    /// `priced_in`: only the SKUs priced in this book (P-D-246).
    pub priced_in: Option<Uuid>,
    /// `not_priced_in`: only the SKUs not priced in this book (P-D-246).
    pub not_priced_in: Option<Uuid>,
    /// `not_in_revision`: only the SKUs this plan revision does not name (P-D-246).
    pub not_in_revision: Option<Uuid>,
}
impl ListParams {
    /// Whether a usage filter asks pricing's sets.
    const fn filters_usage(&self) -> bool {
        self.priced.is_some() || self.in_plan.is_some()
    }
}

/// The plain keys the list takes: the pager's aliases, `q`, the usage filters (P-D-212) and the
/// picker keys (P-D-246).
const LIST_KEYS: &[&str] = &[
    "limit",
    "cursor",
    "q",
    "priced",
    "in_plan",
    "priced_in",
    "not_priced_in",
    "not_in_revision",
];
/// The plain keys the counts take: the list's narrowing without the pager's.
const COUNT_KEYS: &[&str] = &[
    "q",
    "priced",
    "in_plan",
    "priced_in",
    "not_priced_in",
    "not_in_revision",
];
/// The `$` options the list takes, as the extractor binds them (`limit` and `cursor` are their
/// aliases).
const LIST_OPTIONS: &[&str] = &["$filter", "$orderby", "$top", "$skiptoken"];

/// The query keys a door takes besides the extractor's: `plain` without a `$`, and — when
/// `dollar` is given — the only `$` options it takes (otherwise the extractor polices them).
/// The rest is 400, every offender at once (a products copy of ledger's
/// `reject_non_odata_list_params_allowing`). A plain key given twice is 400. An empty `q` is no
/// search. The SKU history and the category list reuse it (P-D-213, P-D-215).
pub(super) fn params(
    query: RawQuery,
    plain: &[&str],
    dollar: Option<&[&str]>,
) -> Result<ListParams, CanonicalError> {
    let Query(pairs) = query.map_err(|e| {
        OdataError::invalid_argument()
            .with_field_violation("query", e.body_text(), "INVALID_QUERY_PARAMS")
            .create()
    })?;
    let takes = plain
        .iter()
        .chain(dollar.unwrap_or(LIST_OPTIONS))
        .map(|k| format!("`{k}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut offenders: Vec<(&str, String, &'static str)> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for (key, _) in &pairs {
        let key = key.as_str();
        let allowed = if key.starts_with('$') {
            dollar.is_none_or(|d| d.contains(&key))
        } else {
            plain.contains(&key)
        };
        if offenders.iter().any(|(k, ..)| *k == key) {
            continue;
        }
        if !allowed {
            offenders.push((
                key,
                format!("`{key}` is not a parameter of this read; it takes {takes}"),
                UNSUPPORTED,
            ));
        } else if !key.starts_with('$') && seen.contains(&key) {
            offenders.push((
                key,
                format!("`{key}` is given more than once"),
                "INVALID_QUERY_PARAMS",
            ));
        } else {
            seen.push(key);
        }
    }
    refused(&offenders)?;
    let value = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let mut malformed: Vec<(&str, String, &'static str)> = Vec::new();
    let mut flag = |name: &'static str| match value(name).as_deref() {
        None => None,
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(other) => {
            malformed.push((
                name,
                format!("`{name}` is `true` or `false`, not `{other}`"),
                "INVALID_QUERY_PARAMS",
            ));
            None
        }
    };
    let (priced, in_plan) = (flag("priced"), flag("in_plan"));
    let mut id = |name: &'static str| match value(name) {
        None => None,
        Some(raw) => raw.parse::<Uuid>().map_or_else(
            |_| {
                malformed.push((
                    name,
                    format!("`{name}` is one id, not `{raw}`"),
                    "INVALID_QUERY_PARAMS",
                ));
                None
            },
            Some,
        ),
    };
    let (priced_in, not_priced_in, not_in_revision) =
        (id("priced_in"), id("not_priced_in"), id("not_in_revision"));
    if priced_in.is_some() && not_priced_in.is_some() {
        malformed.push((
            "not_priced_in",
            "at most one of `priced_in` and `not_priced_in`".to_owned(),
            "INVALID_QUERY_PARAMS",
        ));
    }
    refused(&malformed)?;
    Ok(ListParams {
        q: value("q").filter(|q| !q.is_empty()),
        priced,
        in_plan,
        priced_in,
        not_priced_in,
        not_in_revision,
    })
}

/// The `$filter`, checked the way the pager will read it: only [`SkuFilterField`]s (`null` only
/// on a nullable one), its `archived` terms ones the list can take apart (P-D-263), and each other
/// value through the mapping (a closed value). The condition the rest becomes, for a count.
fn checked_filter(filter: Option<&Expr>) -> Result<Option<sea_orm::Condition>, CanonicalError> {
    let Some(expr) = filter else {
        return Ok(None);
    };
    convert_expr_to_filter_node::<SkuFilterField>(expr)
        .map_err(|e| ODataError::InvalidFilter(e.to_string()))?;
    // P-D-263: the `archived` terms leave before the pager's reading; the list applies them on the
    // mark, and the counts count both sides.
    let (rest, _) = repo::take_archived(Some(expr.clone()))?;
    let Some(rest) = rest else {
        return Ok(None);
    };
    let node = convert_expr_to_filter_node::<SkuListField>(&rest)
        .map_err(|e| ODataError::InvalidFilter(e.to_string()))?;
    let condition = filter_node_to_condition::<SkuListField, SkuListMapping>(&node)
        .map_err(ODataError::InvalidFilter)?;
    Ok(Some(condition))
}

/// The cursor's filter hash over everything that narrows the list: the extractor's hash of
/// `$filter`, `q`, `priced`, `in_plan` and each picker key given (P-D-246). A key not given is
/// left out, so a list without picker keys hashes as it did before them.
fn list_hash(odata: &ODataQuery, params: &ListParams) -> String {
    let mut narrowing = serde_json::json!({
        "filter": odata.filter_hash,
        "q": params.q,
        "priced": params.priced,
        "in_plan": params.in_plan,
    });
    for (key, id) in [
        ("priced_in", params.priced_in),
        ("not_priced_in", params.not_priced_in),
        ("not_in_revision", params.not_in_revision),
    ] {
        if let (Some(id), Some(fields)) = (id, narrowing.as_object_mut()) {
            fields.insert(key.to_owned(), serde_json::Value::from(id.to_string()));
        }
    }
    cursor_hash(&narrowing)
}

/// A cursor's filter hash over `narrowing`: the first 8 bytes of the SHA-256 of its canonical
/// rendering, as hex. A cursor replayed under another narrowing is 400 `FILTER_MISMATCH`.
pub(super) fn cursor_hash(narrowing: &serde_json::Value) -> String {
    content_digest(&canonical_rendering(narrowing))
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut hex, b| {
            const DIGITS: &[u8; 16] = b"0123456789abcdef";
            hex.push(char::from(DIGITS[usize::from(b >> 4)]));
            hex.push(char::from(DIGITS[usize::from(b & 0x0f)]));
            hex
        })
}

/// The narrowing the repository applies before `$filter`: `q`, each usage filter against
/// pricing's sets (asked once, only when a usage filter is given), and each picker key against its
/// scope's set (one `sku_ids_in` call per key given, P-D-246).
async fn list_filter(
    state: &ApiState,
    ctx: &SecurityContext,
    params: &ListParams,
) -> Result<SkuListFilter, CanonicalError> {
    let book_key = match (params.priced_in, params.not_priced_in) {
        (Some(book), _) => Some((true, book)),
        (None, Some(book)) => Some((false, book)),
        (None, None) => None,
    };
    let revision_key = params.not_in_revision;
    let (sets, book, revision) = tokio::try_join!(
        async {
            if params.filters_usage() {
                usage::sets(state, ctx).await
            } else {
                Ok(SkuUsageSets::default())
            }
        },
        async {
            match book_key {
                Some((member, book)) => Ok(Some(SetFilter {
                    member,
                    ids: usage::scoped(state, ctx, UsageScope::Book(book)).await?,
                })),
                None => Ok(None),
            }
        },
        async {
            match revision_key {
                Some(revision) => Ok(Some(SetFilter {
                    member: false,
                    ids: usage::scoped(state, ctx, UsageScope::Revision(revision)).await?,
                })),
                None => Ok(None),
            }
        },
    )?;
    let set = |member: Option<bool>, ids: &[Uuid]| {
        member.map(|member| SetFilter {
            member,
            ids: ids.to_vec(),
        })
    };
    Ok(SkuListFilter {
        text: params.q.clone(),
        priced: set(params.priced, &sets.priced),
        in_plan: set(params.in_plan, &sets.in_plan),
        book,
        revision,
    })
}

/// @cpt-cf-bss-products-fr-read-model
async fn list_skus(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    query: RawQuery,
    odata: Result<OData, CanonicalError>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // Authorization first, then the query (a 403 before a 400).
    let scope = read_scope(&enforcer, &ctx).await?;
    let params = params(query, LIST_KEYS, None)?;
    let OData(mut odata) = odata?;
    if odata.select.is_some() {
        refused(&[(
            "$select",
            "a SKU list item is not projected; drop `$select`".to_owned(),
            UNSUPPORTED,
        )])?;
    }
    checked_filter(odata.filter.as_deref())?;
    for key in &odata.order.0 {
        if SkuOrderField::from_name(&key.field).is_none() {
            return Err(ODataError::InvalidOrderByField(key.field.clone()).into());
        }
    }
    let hash = list_hash(&odata, &params);
    if let Some(cursor) = &odata.cursor
        && cursor.f.as_deref() != Some(hash.as_str())
    {
        return Err(ODataError::FilterMismatch.into());
    }
    odata.filter_hash = Some(hash);
    // The query is valid; now pricing's sets, if a usage filter needs them (P-D-212).
    let filter = list_filter(&state, &ctx, &params).await?;
    let tenant = ctx.subject_tenant_id();
    let ttl = state.fence_ttl_minutes;
    let backend = state.db.db().backend();
    let page = state
        .db
        .db()
        .transaction_with_retry(category_tx_config(&state), contention_db_err, move |tx| {
            let (scope, filter, odata) = (scope.clone(), filter.clone(), odata.clone());
            Box::pin(async move {
                expire(tx, &scope, tenant, ttl).await?;
                repo::page_skus(tx, &scope, tenant, backend, &filter, &odata)
                    .await
                    .map_err(|e| match e {
                        SkuListError::Query(e) => TxError::OData(e),
                        SkuListError::Repo(e) => TxError::Repo(e),
                    })
            })
        })
        .await
        .map_err(tx_to_canonical)?;
    // P-D-197: one call of pricing's usage port for the page, after the page's transaction.
    let ids: Vec<Uuid> = page.items.iter().map(|s| s.id).collect();
    let mut usage = super::usage::of(&state, &ctx, &ids).await;
    let mut items: Vec<SkuListItem> = page
        .items
        .into_iter()
        .map(|s| {
            let counted = usage.remove(&s.id);
            SkuListItem {
                sku: s.into(),
                usage: counted,
            }
        })
        .collect();
    // P-D-262: the page's creators in one lookup; the weak tag covers their names.
    state.actor_names.fill(&ctx, &mut items).await;
    Ok(respond(
        &headers,
        &Page {
            items,
            page_info: page.page_info,
        },
        PRIVATE_REVALIDATE,
    ))
}

/// Recover the tenant's orphan fences before a read, in the read's transaction, so the list
/// and the counts agree on a pending retire (P-D-248: `in_review`, the lifecycle unchanged); each fence lifted is the system's act, with its
/// audit row (P-D-213). Set-based (P-D-211): the fences, one lift, one insert of their rows —
/// the same statements for one expired fence as for fifty; one read when there is none.
async fn expire(
    tx: &impl toolkit_db::secure::DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    ttl: u32,
) -> Result<(), TxError> {
    let now = crate::infra::storage::stored_now();
    let expired = repo::expire_orphan_fences(
        tx,
        scope,
        tenant,
        now - time::Duration::minutes(i64::from(ttl)),
    )
    .await
    .map_err(TxError::Repo)?;
    super::governance::expiry_audits(tx, tenant, &expired, ttl, now).await
}

/// @cpt-cf-bss-products-fr-read-model
async fn count_skus(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    query: RawQuery,
    odata: Result<OData, CanonicalError>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = read_scope(&enforcer, &ctx).await?;
    let params = params(query, COUNT_KEYS, Some(&["$filter"]))?;
    let OData(odata) = odata?;
    // The whole filter is checked as the list would read it, then its lifecycle terms go.
    checked_filter(odata.filter.as_deref())?;
    // Pulled-out lifecycle terms, text functions included, are dropped (the counts count every
    // lifecycle). A term the CASE does not serve is the same 400 the list answers (P-D-249,
    // P-D-264).
    let condition = match odata.filter.as_deref() {
        Some(expr) => {
            let (rest, _) =
                repo::take_lifecycle(expr.clone()).map_err(ODataError::InvalidFilter)?;
            match rest.as_ref() {
                Some(rest) => checked_filter(Some(rest))?,
                None => None,
            }
        }
        None => None,
    };
    let filter = list_filter(&state, &ctx, &params).await?;
    let tenant = ctx.subject_tenant_id();
    let ttl = state.fence_ttl_minutes;
    let backend = state.db.db().backend();
    let counts = state
        .db
        .db()
        .transaction_with_retry(category_tx_config(&state), contention_db_err, move |tx| {
            let (scope, filter, condition) = (scope.clone(), filter.clone(), condition.clone());
            Box::pin(async move {
                expire(tx, &scope, tenant, ttl).await?;
                repo::count_skus(tx, &scope, tenant, backend, &filter, condition)
                    .await
                    .map_err(TxError::Repo)
            })
        })
        .await
        .map_err(tx_to_canonical)?;
    let body: ProductsSkuCounts = counts.into();
    Ok(respond(&headers, &body, PRIVATE_REVALIDATE))
}

#[cfg(test)]
#[path = "sku_list_tests.rs"]
mod sku_list_tests;

#[cfg(test)]
#[path = "conditional_reads_tests.rs"]
mod conditional_reads_tests;
