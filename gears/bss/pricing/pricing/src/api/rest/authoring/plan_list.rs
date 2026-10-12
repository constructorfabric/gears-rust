//! `GET /plans` and `GET /plans/counts` on the stored plan summary (D-484, D-485).
//!
//! The list pages on the toolkit's `OData` pager. `$filter` names `code`, `name`, `book_id`,
//! `currency` and `last_activity_at` (`id` is the tie-break). `$orderby` names `code` (the
//! default), `name` and `last_activity_at`, and `id` breaks the tie in that direction. `limit`
//! (alias `$top`) defaults to 500 and is clamped at 500. The plain keys are `q`, `selling`,
//! `change` and `sku_id`. The cursor's hash covers `$filter` and those keys, so another narrowing
//! is 400 `FILTER_MISMATCH`. The counts use the same narrowing, without the page or the order.
use super::{
    plans,
    support::{
        authz_failure, if_none_match, invalid_because, require_authenticated, revalidate_header,
        transaction, weak_etag_header,
    },
};
use crate::{
    authz::{self, actions, resource_types},
    infra::storage::repo::plan_repo::{PlanListField, PlanListFilter, PlanOrderField},
};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{
    Extension, Router,
    http::{HeaderMap, StatusCode, Uri},
    response::Response,
};
use bss_rest::conditional_get::{PRIVATE_REVALIDATE, respond};
use std::collections::BTreeSet;
use std::sync::Arc;
use toolkit::api::{
    OpenApiRegistry,
    odata::OData,
    operation_builder::{OperationBuilder, OperationBuilderODataExt},
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{
    Error as ODataError,
    filter::{FieldKind, FilterField, convert_expr_to_filter_node},
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The fields a plans `$filter` names. `id` stays the tie-break and is not a filter (D-485).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlanFilterField {
    Code,
    Name,
    BookId,
    Currency,
    LastActivityAt,
}
impl PlanFilterField {
    const fn field(self) -> PlanListField {
        match self {
            Self::Code => PlanListField::Code,
            Self::Name => PlanListField::Name,
            Self::BookId => PlanListField::BookId,
            Self::Currency => PlanListField::Currency,
            Self::LastActivityAt => PlanListField::LastActivityAt,
        }
    }
}
impl FilterField for PlanFilterField {
    const FIELDS: &'static [Self] = &[
        Self::Code,
        Self::Name,
        Self::BookId,
        Self::Currency,
        Self::LastActivityAt,
    ];
    fn name(&self) -> &'static str {
        self.field().name()
    }
    fn kind(&self) -> FieldKind {
        self.field().kind()
    }
    fn nullable(&self) -> bool {
        self.field().nullable()
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}

/// Register the list and, before `GET /plans/{id}`, the counts. Both under `plan × read`.
pub(super) fn register(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::get("/bss-pricing/v1/plans")
        .operation_id("bss_pricing.list_plans")
        .summary("List the plans")
        .description(
            "One page of the tenant's plans (D-485). Each plan carries selling and change for \
             the request's day, derived from its stored summary (D-484), last_activity_at (the \
             instant a page ordered by it is ordered by), the headers of its revisions as they \
             read today (D-447), each header with book { id, code, name, currency } beside \
             book_id (D-516), its current revision with that revision's book (id, code, name, \
             currency, valid_from and valid_until, D-515) and the published revision in effect \
             (D-460). OData `$filter` over code, \
             name, book_id, currency and last_activity_at; `$orderby` over code (the default), \
             name and last_activity_at, with id breaking the tie in that direction; `$top` \
             (alias limit; default 500, clamped at 500) and cursor (alias $skiptoken). q is a \
             case-insensitive literal substring of the code or the name. selling is true or \
             false. change is none, draft, pending or scheduled, one or several, comma-separated. \
             sku_id keeps the plans whose draft, pending, scheduled or published revisions name \
             the SKU through an entry (D-434), inside the page query. Five statements for a \
             non-empty page, whatever its size. A matching If-None-Match is 304 with an empty body; \
             the 200 carries a weak ETag of its JSON and Cache-Control private, no-cache (D-518). \
             Refusals: 400 QUERY_INVALID for any other plain \
             key, a repeated key, or a malformed value; 400 FILTER_MISMATCH for a cursor replayed \
             under another narrowing; 400 for $select, $count and an order field it does not take.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .query_param_typed(
            "limit",
            false,
            "Page size, alias of $top (default 500, clamped at 500)",
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
        .query_param_typed("selling", false, "true or false", "boolean")
        .query_param_typed(
            "change",
            false,
            "none, draft, pending, scheduled: one or several, comma-separated",
            "string",
        )
        .query_param_typed(
            "sku_id",
            false,
            "Only the plans naming this SKU through an entry",
            "string",
        )
        .param(if_none_match())
        .handler(list_plans)
        .with_odata_filter::<PlanFilterField>()
        .with_odata_orderby::<PlanOrderField>()
        .json_response_with_schema::<crate::api::rest::authoring::dto::PricingPlanList>(
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
        .register(router, openapi);
    OperationBuilder::get("/bss-pricing/v1/plans/counts")
        .operation_id("bss_pricing.count_plans")
        .summary("Count the plans")
        .description(
            "Counts the tenant's plans that the list's narrowing keeps (D-485): by_selling \
             (true and false; they add up to total), by_change (none, draft, pending, \
             scheduled; each 0 when none) and total. The same plain keys and $filter as the list, \
             and not limit, cursor or $orderby. One grouped statement. A matching If-None-Match is \
             304 with an empty body; the 200 carries a weak ETag of its JSON and Cache-Control \
             private, no-cache (D-518). Refusals: the list's.",
        )
        .tag("Pricing")
        .authenticated()
        .no_license_required()
        .query_param_typed(
            "q",
            false,
            "Case-insensitive substring of the code or the name",
            "string",
        )
        .query_param_typed("selling", false, "true or false", "boolean")
        .query_param_typed(
            "change",
            false,
            "none, draft, pending, scheduled: one or several, comma-separated",
            "string",
        )
        .query_param_typed(
            "sku_id",
            false,
            "Only the plans naming this SKU through an entry",
            "string",
        )
        .param(if_none_match())
        .handler(count_plans)
        .with_odata_filter::<PlanFilterField>()
        .json_response_with_schema::<crate::api::rest::authoring::dto::PricingPlanCounts>(
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

const LIST_PLAIN: &[&str] = &["limit", "cursor", "q", "selling", "change", "sku_id"];
const COUNT_PLAIN: &[&str] = &["q", "selling", "change", "sku_id"];
const CHANGES: &[&str] = &["none", "draft", "pending", "scheduled"];

struct Params {
    q: Option<String>,
    selling: Option<bool>,
    change: Vec<String>,
    sku_id: Option<Uuid>,
}

fn params(uri: &Uri, plain: &[&str]) -> Result<Params, CanonicalError> {
    let axum::extract::Query(pairs) =
        axum::extract::Query::<Vec<(String, String)>>::try_from_uri(uri)
            .map_err(|_| invalid_because("query", "QUERY_INVALID", "a malformed query string"))?;
    super::support::plain_keys(
        &pairs,
        plain,
        |key| key.starts_with('$'),
        |key| format!("`{key}` is not a parameter of this read"),
    )?;
    let value = |name: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let selling = value("selling")
        .filter(|v| !v.is_empty())
        .map(|raw| match raw.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(invalid_because(
                "selling",
                "QUERY_INVALID",
                "`selling` is true or false",
            )),
        })
        .transpose()?;
    let change = value("change")
        .filter(|v| !v.is_empty())
        .map(|raw| {
            let mut tokens = BTreeSet::new();
            for token in raw.split(',') {
                if !CHANGES.contains(&token) {
                    return Err(invalid_because(
                        "change",
                        "QUERY_INVALID",
                        "`change` is none, draft, pending or scheduled",
                    ));
                }
                tokens.insert(token.to_owned());
            }
            Ok(tokens.into_iter().collect())
        })
        .transpose()?
        .unwrap_or_default();
    let sku_id = value("sku_id")
        .map(|raw| {
            raw.parse::<Uuid>()
                .map_err(|_| invalid_because("sku_id", "QUERY_INVALID", "`sku_id` is a SKU id"))
        })
        .transpose()?;
    let q = value("q").filter(|q| !q.is_empty());
    if let Some(q) = q.as_deref() {
        super::caps::search(q)?;
    }
    Ok(Params {
        q,
        selling,
        change,
        sku_id,
    })
}

fn list_hash(odata: &toolkit_odata::ODataQuery, params: &Params) -> Result<String, CanonicalError> {
    super::support::page_hash(&serde_json::json!({
        "filter": odata.filter_hash,
        "q": params.q,
        "selling": params.selling,
        "change": params.change,
        "sku_id": params.sku_id,
    }))
}

fn prepared(
    uri: &Uri,
    plain: &[&str],
    odata: Result<OData, CanonicalError>,
    today: time::Date,
) -> Result<(PlanListFilter, toolkit_odata::ODataQuery), CanonicalError> {
    let params = params(uri, plain)?;
    let OData(mut odata) = odata?;
    if odata.select.is_some() {
        return Err(toolkit_odata::errors::OdataError::invalid_argument()
            .with_field_violation(
                "$select",
                "a plan is not projected; drop `$select`",
                "UNSUPPORTED_QUERY_PARAM",
            )
            .create());
    }
    if let Some(expr) = odata.filter.as_deref() {
        convert_expr_to_filter_node::<PlanFilterField>(expr)
            .map_err(|e| ODataError::InvalidFilter(e.to_string()))?;
    }
    for key in &odata.order.0 {
        if PlanListField::from_name(&key.field).is_none_or(|field| !field.orderable()) {
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
    Ok((
        PlanListFilter {
            text: params.q,
            sku: params.sku_id,
            selling: params.selling,
            change: params.change,
            today,
        },
        odata,
    ))
}

async fn list_plans(
    Extension(state): Extension<Arc<super::AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    uri: Uri,
    odata: Result<OData, CanonicalError>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PLAN,
        actions::READ,
        None,
        None,
    )
    .await
    .map_err(authz_failure)?;
    let today = state.clock.now().date();
    let (filter, odata) = prepared(&uri, LIST_PLAIN, odata, today)?;
    let tenant = ctx.subject_tenant_id();
    let backend = state.db.db().backend();
    let mut body = transaction(&state.db.db(), move |tx| {
        let (scope, filter, odata) = (scope.clone(), filter.clone(), odata.clone());
        Box::pin(async move { plans::list(tx, &scope, tenant, backend, &filter, &odata).await })
    })
    .await?;
    // D-519: the page's actors in one lookup, after the transaction; the tag covers the names.
    state.actor_names.fill(&ctx, &mut body).await;
    Ok(respond(&headers, &body, PRIVATE_REVALIDATE))
}

async fn count_plans(
    Extension(state): Extension<Arc<super::AuthoringState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    uri: Uri,
    odata: Result<OData, CanonicalError>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::PLAN,
        actions::READ,
        None,
        None,
    )
    .await
    .map_err(authz_failure)?;
    let today = state.clock.now().date();
    let (filter, odata) = prepared(&uri, COUNT_PLAIN, odata, today)?;
    if !odata.order.0.is_empty() {
        return Err(invalid_because(
            "$orderby",
            "QUERY_INVALID",
            "the counts take the list's narrowing, not an order",
        ));
    }
    let tenant = ctx.subject_tenant_id();
    let backend = state.db.db().backend();
    transaction(&state.db.db(), move |tx| {
        let (scope, filter, odata, headers) = (
            scope.clone(),
            filter.clone(),
            odata.clone(),
            headers.clone(),
        );
        Box::pin(async move {
            let body = plans::counts(tx, &scope, tenant, backend, &filter, &odata).await?;
            Ok(respond(&headers, &body, PRIVATE_REVALIDATE))
        })
    })
    .await
}
