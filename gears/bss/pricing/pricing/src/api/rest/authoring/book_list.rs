//! `GET /price-books` on the toolkit's `OData` pager (D-442), each book with its stats (D-441).
//!
//! The list takes `$filter` over [`BookFilterField`] (`id`, `code`, `name`, `currency`,
//! `valid_from`, `valid_until`; the two dates compare with `null`), `$orderby` over [`BookOrderField`] (`code`,
//! `name`; tie-break `id`), `$top` (alias `limit`; default 200, clamped at 500) and `cursor`
//! (alias `$skiptoken`), plus `q` (a case-insensitive substring of the code or the name) and
//! `sku_id` (the books with an entry of that SKU). The cursor carries a hash of `$filter`, `q` and
//! `sku_id`, so a cursor replayed under another narrowing is 400 `FILTER_MISMATCH`. Any other
//! plain key, a repeated one or a malformed `sku_id` is 400 `QUERY_INVALID`; the pager refuses
//! `$select`, `$count` and the other `$` options with `UNSUPPORTED_QUERY_PARAM`.
use super::{
    AuthoringState, books,
    dto::PricingPriceBookReadDto,
    support::{
        authz_failure, if_none_match, invalid_because, require_authenticated, revalidate_header,
        transaction, weak_etag_header,
    },
};
use crate::{
    authz::{self, actions, resource_types},
    infra::storage::repo::book_repo::{BookListField, BookListFilter},
};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{
    Extension, Router,
    http::{HeaderMap, StatusCode, Uri},
    response::Response,
};
use bss_rest::conditional_get::{PRIVATE_REVALIDATE, respond};
use std::sync::Arc;
use toolkit::api::{
    OpenApiRegistry,
    odata::OData,
    operation_builder::{OperationBuilder, OperationBuilderODataExt},
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{
    Error as ODataError, ODataQuery, Page,
    errors::OdataError,
    filter::{FieldKind, FilterField, convert_expr_to_filter_node},
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The fields a list `$filter` names, `id` included (D-480).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BookFilterField {
    Id,
    Code,
    Name,
    Currency,
    ValidFrom,
    ValidUntil,
    /// The archive mark (D-522): `archived eq true` lists only the archived books.
    Archived,
}
impl BookFilterField {
    const fn field(self) -> BookListField {
        match self {
            Self::Id => BookListField::Id,
            Self::Code => BookListField::Code,
            Self::Name => BookListField::Name,
            Self::Currency => BookListField::Currency,
            Self::ValidFrom => BookListField::ValidFrom,
            Self::ValidUntil => BookListField::ValidUntil,
            Self::Archived => BookListField::Archived,
        }
    }
}
impl FilterField for BookFilterField {
    const FIELDS: &'static [Self] = &[
        Self::Id,
        Self::Code,
        Self::Name,
        Self::Currency,
        Self::ValidFrom,
        Self::ValidUntil,
        Self::Archived,
    ];
    fn name(&self) -> &'static str {
        self.field().name()
    }
    fn kind(&self) -> FieldKind {
        self.field().kind()
    }
    /// `valid_from` and `valid_until`: `eq null` is a book open on that side.
    fn nullable(&self) -> bool {
        self.field().nullable()
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}
/// The fields a list `$orderby` names; `id` is the tie-break every order ends with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BookOrderField {
    Code,
    Name,
    Id,
}
impl BookOrderField {
    const fn field(self) -> BookListField {
        match self {
            Self::Code => BookListField::Code,
            Self::Name => BookListField::Name,
            Self::Id => BookListField::Id,
        }
    }
}
impl FilterField for BookOrderField {
    const FIELDS: &'static [Self] = &[Self::Code, Self::Name, Self::Id];
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

/// Register the list under `price_book × read`.
pub(super) fn register(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    OperationBuilder::get("/bss-pricing/v1/price-books")
        .operation_id("bss_pricing.list_books")
        .summary("List, filter and search the price books")
        .description(
            "One page of the tenant's price books (D-442), each with its stats (D-441): its \
             entries and their distinct SKUs, the distinct plans with a draft, pending, scheduled \
             or published revision on it and those that name it only through superseded revisions, \
             its prices by state (the approved ones also as scheduled, active and superseded \
             today), its prices units in review and its last change. OData `$filter` over id, \
             code, name, currency, valid_from and valid_until (`eq null`: open on that side), and \
             archived: an archived book is left out unless asked `archived eq true` (D-522), and \
             `archived` compares with `eq` or `ne` and a boolean, joined only by top-level `and`; \
             `$orderby` \
             over code and name (tie-break id; default code); `$top` (alias `limit`; default 200, \
             clamped at 500) and `cursor` (alias `$skiptoken`) from `page_info`. `q` is a \
             case-insensitive substring of the code or the name, matched literally; `sku_id` keeps \
             the books with an entry of that SKU. Refusals: 400 QUERY_INVALID for any other key, a \
             repeated key or a malformed sku_id; 400 FILTER_MISMATCH for a cursor replayed with \
             another `$filter`, `q` or `sku_id`; 400 for `$select`, `$count` and the other OData \
             options it does not take. A matching If-None-Match is 304 with an empty body; the 200 \
             carries a weak ETag of its JSON and Cache-Control private, no-cache (D-518).",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .query_param_typed(
            "limit",
            false,
            "Page size, alias of $top (default 200, clamped at 500)",
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
            "Case-insensitive substring of the code or the name",
            "string",
        )
        .query_param_typed(
            "sku_id",
            false,
            "Only the books with an entry of this SKU",
            "string",
        )
        .param(if_none_match())
        .handler(list_books)
        .with_odata_filter::<BookFilterField>()
        .with_odata_orderby::<BookOrderField>()
        .json_response_with_schema::<Page<PricingPriceBookReadDto>>(
            openapi,
            StatusCode::OK,
            "Response",
        )
        .response_header(weak_etag_header())
        .response_header(revalidate_header())
        .no_content_response(
            StatusCode::NOT_MODIFIED,
            "The If-None-Match tag matches this body",
        )
        .response_header(weak_etag_header())
        .response_header(revalidate_header())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// What the list reads besides the `OData` options.
#[derive(Debug, Clone, Default)]
struct ListParams {
    q: Option<String>,
    sku_id: Option<Uuid>,
}
/// The plain keys the list takes; the `$` options are the extractor's.
const PLAIN: &[&str] = &["limit", "cursor", "q", "sku_id"];

/// The query's plain keys: only [`PLAIN`], each once, and a well-formed `sku_id`; an empty `q` is
/// no search. Any other plain key is 400 `QUERY_INVALID`, as pricing's other reads refuse one.
fn params(uri: &Uri) -> Result<ListParams, CanonicalError> {
    let axum::extract::Query(pairs) =
        axum::extract::Query::<Vec<(String, String)>>::try_from_uri(uri)
            .map_err(|_| invalid_because("query", "QUERY_INVALID", "a malformed query string"))?;
    super::support::plain_keys(
        &pairs,
        PLAIN,
        |key| key.starts_with('$'),
        |key| {
            format!(
                "`{key}` is not a parameter of this read; it takes limit, cursor, q, sku_id and the OData options"
            )
        },
    )?;
    let value = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let sku_id = value("sku_id")
        .map(|raw| {
            raw.parse::<Uuid>()
                .map_err(|_| invalid_because("sku_id", "QUERY_INVALID", "`sku_id` is a SKU id"))
        })
        .transpose()?;
    Ok(ListParams {
        q: {
            let q = value("q").filter(|q| !q.is_empty());
            if let Some(q) = q.as_deref() {
                super::caps::search(q)?;
            }
            q
        },
        sku_id,
    })
}

/// The cursor's filter hash over everything that narrows the list: the extractor's hash of
/// `$filter`, `q` and `sku_id` — the first 8 bytes of the SHA-256 of their canonical JSON, as hex.
fn list_hash(odata: &ODataQuery, params: &ListParams) -> Result<String, CanonicalError> {
    super::support::page_hash(&serde_json::json!({
        "filter": odata.filter_hash,
        "q": params.q,
        "sku_id": params.sku_id,
    }))
}

async fn list_books(
    Extension(state): Extension<Arc<AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    uri: Uri,
    odata: Result<OData, CanonicalError>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    // Authorization first, then the query (a 403 before a 400).
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PRICE_BOOK,
        actions::READ,
        None,
        None,
    )
    .await
    .map_err(authz_failure)?;
    let params = params(&uri)?;
    let OData(mut odata) = odata?;
    if odata.select.is_some() {
        return Err(OdataError::invalid_argument()
            .with_field_violation(
                "$select",
                "a book is not projected; drop `$select`",
                "UNSUPPORTED_QUERY_PARAM",
            )
            .create());
    }
    if let Some(expr) = odata.filter.as_deref() {
        convert_expr_to_filter_node::<BookFilterField>(expr)
            .map_err(|e| ODataError::InvalidFilter(e.to_string()))?;
        // D-522: an `archived` term the list cannot take apart is 400 before any read.
        bss_rest::archived::take_archived(expr.clone()).map_err(ODataError::InvalidFilter)?;
    }
    for key in &odata.order.0 {
        if BookOrderField::from_name(&key.field).is_none() {
            return Err(ODataError::InvalidOrderByField(key.field.clone()).into());
        }
    }
    let hash = list_hash(&odata, &params)?;
    if let Some(cursor) = &odata.cursor
        && cursor.f.as_deref() != Some(hash.as_str())
    {
        return Err(ODataError::FilterMismatch.into());
    }
    odata.filter_hash = Some(hash);
    let filter = BookListFilter {
        text: params.q,
        sku: params.sku_id,
    };
    let tenant = ctx.subject_tenant_id();
    let backend = state.db.db().backend();
    let today = time::OffsetDateTime::now_utc().date();
    let mut page = transaction(&state.db.db(), move |tx| {
        let (scope, filter, odata) = (scope.clone(), filter.clone(), odata.clone());
        Box::pin(
            async move { books::page(tx, &scope, tenant, backend, &filter, &odata, today).await },
        )
    })
    .await?;
    // D-522: the page's archiving actors in one lookup (D-519); the weak tag covers their names.
    state.actor_names.fill(&ctx, &mut page.items).await;
    Ok(respond(&headers, &page, PRIVATE_REVALIDATE))
}
