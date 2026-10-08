//! The three early S6-01 read routes (08 §3.3; S1-02 catalog `get`, `list`, `list_lines`):
//! `GET /orders/{orderId}`, `GET /orders` and `GET /orders/{orderId}/lines`.
//!
//! Boundary order: `X-Delegation-Proof-Ref` (`request-invalid`, D-202) → `page_size`
//! (`page-size-exceeded`) → filters and unknown query names (`filter-invalid`) → `cursor`
//! (`cursor-invalid`, D-139). All of these precede the access decision and append no
//! access-log row (08 §3.6 common read wrapper item 1). Each handler then enters the shared
//! [`ReadService`], which the local SDK uses too. Successful per-order reads carry the current
//! commercial version as the strong `ETag` (the value `If-Match` sends back on a write); the
//! bodies are `snake_case` (D-206). Refusals are RFC 9457 problems from the Orders registry.
use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Router};
use bss_orders_lifecycle_sdk::OrdersError;
use bss_orders_lifecycle_sdk::catalog::{OrderState, Reason};
use bss_orders_lifecycle_sdk::models::OrderVersion;
use bss_orders_lifecycle_sdk::reads::{Cursor, LineList, ListOrders, OrderFilters, PageSize};
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{OperationBuilder, ResponseHeaderSpec, ResponseHeaderType};
use toolkit::api::rest::extract::Path;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{OrdersLinePage, OrdersOrderPage, OrdersOrderView};
use crate::authz::Caller;
use crate::infra::read::{self, ReadService};

const TAG: &str = "Orders Lifecycle";
const PAGE_SIZE: &str = "page_size";
const CURSOR: &str = "cursor";
/// The S1-02 order-list filter names (CONTRACTS.md), in documentation order.
const FILTERS: [&str; 5] = [
    "state",
    "created_from",
    "created_to",
    "state_entered_before",
    "contract_id",
];

fn refused(reason: Reason) -> OrdersError {
    OrdersError::Refused(reason)
}

/// The decoded query, with repeated names remembered so each step refuses its own.
struct Query {
    values: BTreeMap<String, String>,
    repeated: Vec<String>,
}

/// Decode the query string; a string that is not `application/x-www-form-urlencoded` is a
/// filter failure (no name can be examined).
fn query(uri: &Uri) -> Result<Query, OrdersError> {
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(uri.query().unwrap_or(""))
        .map_err(|_| refused(Reason::FilterInvalid))?;
    let mut values = BTreeMap::new();
    let mut repeated = Vec::new();
    for (name, value) in pairs {
        if values.insert(name.clone(), value).is_some() {
            repeated.push(name);
        }
    }
    Ok(Query { values, repeated })
}

/// Step 1: `page_size` is an unsigned decimal integer in 1–200; anything else, including a
/// repeated name, is `page-size-exceeded` (the S1-02 oracle's rule for a non-integer value).
fn page_size(query: &Query) -> Result<Option<PageSize>, OrdersError> {
    if query.repeated.iter().any(|name| name == PAGE_SIZE) {
        return Err(refused(Reason::PageSizeExceeded));
    }
    let Some(text) = query.values.get(PAGE_SIZE) else {
        return Ok(None);
    };
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(refused(Reason::PageSizeExceeded));
    }
    let value = text
        .parse::<u64>()
        .map_err(|_| refused(Reason::PageSizeExceeded))?;
    read::page_size(Some(value))
}

fn instant(text: &str) -> Result<time::OffsetDateTime, OrdersError> {
    time::OffsetDateTime::parse(text, &Rfc3339).map_err(|_| refused(Reason::FilterInvalid))
}

/// Step 2 (order list): every query name is `page_size`, `cursor` or a supported filter, each at
/// most once, with a well-formed value; `state` is a registered state token.
fn order_filters(query: &Query) -> Result<OrderFilters, OrdersError> {
    let invalid = || refused(Reason::FilterInvalid);
    for name in query.values.keys() {
        if name != PAGE_SIZE && name != CURSOR && !FILTERS.contains(&name.as_str()) {
            return Err(invalid());
        }
    }
    if query
        .repeated
        .iter()
        .any(|name| FILTERS.contains(&name.as_str()))
    {
        return Err(invalid());
    }
    let get = |name: &str| query.values.get(name).map(String::as_str);
    let state = get("state")
        .map(|token| {
            serde_json::from_value::<OrderState>(serde_json::Value::String(token.to_owned()))
                .map_err(|_| invalid())
        })
        .transpose()?;
    let created_from = get("created_from").map(instant).transpose()?;
    let created_to = get("created_to").map(instant).transpose()?;
    let state_entered_before = get("state_entered_before").map(instant).transpose()?;
    let contract_id = get("contract_id")
        .map(|text| Uuid::try_parse(text).map_err(|_| invalid()))
        .transpose()?;
    Ok(OrderFilters {
        state,
        created_from,
        created_to,
        state_entered_before,
        contract_id,
    })
}

/// Step 2 (line list): the collection takes no filters, so any other name is `filter-invalid`.
fn no_filters(query: &Query) -> Result<(), OrdersError> {
    if query
        .values
        .keys()
        .any(|name| name != PAGE_SIZE && name != CURSOR)
    {
        return Err(refused(Reason::FilterInvalid));
    }
    Ok(())
}

/// Step 3: a present `cursor` is a bounded opaque token; repeated or malformed is
/// `cursor-invalid`. Its binding is validated by the service before the access decision.
fn cursor(query: &Query) -> Result<Option<Cursor>, OrdersError> {
    if query.repeated.iter().any(|name| name == CURSOR) {
        return Err(refused(Reason::CursorInvalid));
    }
    query
        .values
        .get(CURSOR)
        .map(|text| Cursor::try_from(text.clone()).map_err(|_| refused(Reason::CursorInvalid)))
        .transpose()
}

/// The validated order-list request, in the flow's order.
fn list_request(uri: &Uri) -> Result<ListOrders, OrdersError> {
    let query = query(uri)?;
    let page_size = page_size(&query)?;
    let filters = order_filters(&query)?;
    let cursor = cursor(&query)?;
    Ok(ListOrders {
        filters,
        page_size,
        cursor,
    })
}

/// The validated line-list request, in the flow's order.
fn line_request(uri: &Uri) -> Result<LineList, OrdersError> {
    let query = query(uri)?;
    let page_size = page_size(&query)?;
    no_filters(&query)?;
    let cursor = cursor(&query)?;
    Ok(LineList { page_size, cursor })
}

fn etag_value(version: OrderVersion) -> HeaderValue {
    HeaderValue::from_str(&format!("\"{}\"", i64::from(version)))
        .unwrap_or_else(|_| HeaderValue::from_static("\"0\""))
}

fn served<T: Serialize>(etag: Option<OrderVersion>, body: &T) -> Response {
    let mut response = (StatusCode::OK, axum::Json(body)).into_response();
    if let Some(version) = etag {
        response
            .headers_mut()
            .insert(axum::http::header::ETAG, etag_value(version));
    }
    response
}

fn outcome<T: Serialize>(result: Result<(Option<OrderVersion>, T), OrdersError>) -> Response {
    match result {
        Ok((etag, body)) => served(etag, &body),
        Err(error) => super::problem(error).into_response(),
    }
}

async fn get_order(
    Extension(service): Extension<Arc<ReadService>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(order_id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    let run = async {
        let caller = super::caller(ctx, &headers)?;
        let view = service.get(&caller, order_id).await?;
        Ok((Some(view.version.version), view))
    };
    outcome(run.await)
}

async fn list_orders(
    Extension(service): Extension<Arc<ReadService>>,
    Extension(ctx): Extension<SecurityContext>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let run = async {
        let caller: Caller = super::caller(ctx, &headers)?;
        let request = list_request(&uri)?;
        let page = service.list(&caller, &request).await?;
        Ok((None, page))
    };
    outcome(run.await)
}

async fn list_lines(
    Extension(service): Extension<Arc<ReadService>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(order_id): Path<Uuid>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let run = async {
        let caller = super::caller(ctx, &headers)?;
        let request = line_request(&uri)?;
        let page = service.list_lines(&caller, order_id, &request).await?;
        Ok((Some(page.current_version), page))
    };
    outcome(run.await)
}

fn etag() -> ResponseHeaderSpec {
    ResponseHeaderSpec::new(
        "ETag",
        "The current commercial version, the value a write sends back as If-Match",
        ResponseHeaderType::String,
    )
}

const PAGE_SIZE_DOC: &str =
    "Page size 1-200 (default 50); outside the range is 400 PAGE_SIZE_EXCEEDED";
const CURSOR_DOC: &str = "Opaque continuation from the previous page's next_cursor; a token \
                          failing structure, version, precision or its binding to this request \
                          is 400 CURSOR_INVALID";

/// Register the three read routes and attach the shared service once (`ToolKit` REST rule).
#[allow(
    clippy::too_many_lines,
    reason = "one OperationBuilder chain per route keeps every route's contract in one place"
)]
pub fn router(service: Arc<ReadService>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = Router::new();
    let router = OperationBuilder::get("/bss-orders-lifecycle/v1/orders/{orderId}")
        .operation_id("bss_orders_lifecycle.get")
        .summary("Read the composed current order")
        .description(
            "The order at its current commercial version with its current lines, read from the \
             stored aggregate under the caller's PDP scope (never a chain walk, cache or \
             replica). Draft reads carry draft_revision; the ETag is the current version. A \
             missing or hidden order is 404 ORDER_NOT_FOUND on both arms. Committed-version \
             content (pins, totals, fulfillment projection, acceptance, administrative values) \
             is not delivered yet: an order outside draft answers 503. 503 READ_STORE_UNAVAILABLE \
             when a required access log cannot be committed; nothing is disclosed then.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("orderId", "Order identity")
        .param(super::delegation_proof_param())
        .handler(get_order)
        .json_response_with_schema::<OrdersOrderView>(openapi, StatusCode::OK, "Current order")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-orders-lifecycle/v1/orders")
        .operation_id("bss_orders_lifecycle.list")
        .summary("List orders")
        .description(
            "One authorized keyset page in (created_at, order_id) order, scoped by the \
                 caller's PDP relationship (customer resource tenant, seller, current payer or an \
                 explicit service order set) in SQL. Filters: state (exact), created_from \
                 (inclusive), created_to (exclusive), state_entered_before (inclusive: in the \
                 current state since that instant or earlier), contract_id (exact); an unknown \
                 name or malformed value is 400 FILTER_INVALID. Untargeted denials are 403 \
                 DELEGATION_PROOF_REQUIRED / DELEGATION_PROOF_INVALID / \
                 OPERATION_NOT_PERMITTED_FOR_ACTOR.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .query_param_typed(PAGE_SIZE, false, PAGE_SIZE_DOC, "integer")
        .query_param(CURSOR, false, CURSOR_DOC)
        .query_param(
            "state",
            false,
            "Exact registered state token (for example draft)",
        )
        .query_param(
            "created_from",
            false,
            "RFC 3339 instant, inclusive lower bound on created_at",
        )
        .query_param(
            "created_to",
            false,
            "RFC 3339 instant, exclusive upper bound on created_at",
        )
        .query_param(
            "state_entered_before",
            false,
            "RFC 3339 instant: in the current state since that instant or earlier (inclusive)",
        )
        .query_param("contract_id", false, "Exact contract identity")
        .param(super::delegation_proof_param())
        .handler(list_orders)
        .json_response_with_schema::<OrdersOrderPage>(openapi, StatusCode::OK, "Authorized page")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::get("/bss-orders-lifecycle/v1/orders/{orderId}/lines")
        .operation_id("bss_orders_lifecycle.list_lines")
        .summary("List the order's current lines")
        .description(
            "One authorized keyset page of the current lines in (created_at, line_id) \
                 identity order, through the current parent's PDP scope. While the order is a \
                 draft the members are the working set as authored; a removed draft line is not \
                 a member. current_version and draft_revision come from the same snapshot as the \
                 lines; the ETag is the current version. Per-line fulfillment status and \
                 subscription linkage are not delivered yet.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("orderId", "Order identity")
        .query_param_typed(PAGE_SIZE, false, PAGE_SIZE_DOC, "integer")
        .query_param(CURSOR, false, CURSOR_DOC)
        .param(super::delegation_proof_param())
        .handler(list_lines)
        .json_response_with_schema::<OrdersLinePage>(openapi, StatusCode::OK, "Authorized page")
        .response_header(etag())
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    router.layer(Extension(service))
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
