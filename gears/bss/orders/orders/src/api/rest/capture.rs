//! The five draft authoring routes (DESIGN 02 §3.3; S1-02 catalog `create`, `patch_order`,
//! `add_line`, `patch_line`, `remove_line`).
//!
//! Boundary order (S1-02 `contract_checks.py`, CONTRACTS.md): required `If-Match` (428, nothing
//! else touched) → `Idempotency-Key` → `X-Delegation-Proof-Ref` → `expected_draft_revision` →
//! body shape, decoded into the REST DTOs of [`super::dto`] and converted into the SDK request
//! types. Each handler then enters the shared [`CaptureService`], which the local SDK uses
//! too. A settled outcome — success or registered refusal — is rendered exactly as stored, so a
//! same-key replay is byte-identical: the success body is the stored SDK snapshot, never
//! re-encoded through a [`super::dto`] type, which only documents its shape in `OpenAPI`.
//! Bodies are `snake_case` (D-206).
//!
//! Pre-engine throttling (S2-12, D-185): every route here binds the gateway's identity-keyed
//! caller zone, and the four existing-order writes consult the gear-local `(caller, order)`
//! limiter first, before any header, body, authorization or storage activity, so a throttled
//! attempt never reaches the engine or writes a row ([`super::throttle`]).
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::OriginalUri;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Router};
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::authoring::{
    AddLine, AuthoringInvalid, CreateMeta, CreateOrder, HeaderPatch, LinePatch,
};
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::models::{CallMeta, DraftRevision};
use serde_json::{Map, Value};
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{
    OperationBuilder, ParamSpec, ResponseHeaderSpec, ResponseHeaderType,
};
use toolkit::api::rest::extract::Path;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{
    OrdersAddLine, OrdersCreateOrder, OrdersHeaderFields, OrdersHeaderPatch, OrdersLineFields,
    OrdersLinePatch, OrdersOrderView, OrdersRemoveLine, OrdersTransitionResult,
};
use super::error::ApiResult;
use super::throttle::{PerOrderLimiter, caller_write_throttling};
use crate::authz::Caller;
use crate::infra::capture::{CaptureService, Settled, WriteMeta};
use crate::infra::diagnostics::serde_category;

const IDEMPOTENCY_KEY: &str = "idempotency-key";
const IF_MATCH: &str = "if-match";
const IDEMPOTENCY_REPLAYED: &str = "idempotency-replayed";
const EXPECTED_DRAFT_REVISION: &str = "expected_draft_revision";
const TAG: &str = "Orders";
/// 412 for every existing-order write: a stale `If-Match` is settled and audited, kept apart from
/// the 409 draft-revision conflict.
const STALE_IF_MATCH: &str =
    "PRECONDITION_FAILED: If-Match is not the current version (names current_version)";

fn invalid() -> OrdersError {
    OrdersError::Refused(Reason::RequestInvalid)
}

/// `request-invalid` for a body the wire refuses; the Problem is unchanged (`context.data` stays
/// `{}`). The debug record names the operation, the refused member where it is static and the
/// serde category; never the serde message, which quotes the client's input.
fn refused_body(
    operation: &'static str,
    member: Option<&'static str>,
    category: Option<&'static str>,
) -> OrdersError {
    tracing::debug!(
        target: "orders.rest.capture",
        operation,
        member,
        category,
        "request body refused request-invalid"
    );
    invalid()
}

fn single_header<'h>(headers: &'h HeaderMap, name: &str) -> Result<Option<&'h str>, OrdersError> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(invalid());
    }
    first.to_str().map(Some).map_err(|_| invalid())
}

/// `If-Match` → expected version; any malformed or repeated value is the 428 refusal.
fn expected_version(
    headers: &HeaderMap,
) -> Result<bss_orders_sdk::models::OrderVersion, OrdersError> {
    let value = single_header(headers, IF_MATCH)
        .map_err(|_| OrdersError::Refused(Reason::ExpectedVersionRequired))?;
    super::expected_version(value)
}

fn idempotency_key(
    headers: &HeaderMap,
) -> Result<bss_orders_sdk::models::IdempotencyKey, OrdersError> {
    super::idempotency_key(single_header(headers, IDEMPOTENCY_KEY)?)
}

/// The JSON object body; `allow_empty` admits an absent body (line removal).
fn object(
    operation: &'static str,
    body: &[u8],
    allow_empty: bool,
) -> Result<Map<String, Value>, OrdersError> {
    if body.is_empty() && allow_empty {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(refused_body(operation, None, Some("not_an_object"))),
        Err(error) => Err(refused_body(operation, None, Some(serde_category(&error)))),
    }
}

/// Extract the optional independent `expected_draft_revision` member: a nonnegative signed bigint
/// (never a boolean, string or fraction).
fn take_draft_revision(
    operation: &'static str,
    map: &mut Map<String, Value>,
) -> Result<Option<DraftRevision>, OrdersError> {
    let refused = || refused_body(operation, Some(EXPECTED_DRAFT_REVISION), None);
    match map.remove(EXPECTED_DRAFT_REVISION) {
        None => Ok(None),
        Some(Value::Number(n)) => n
            .as_i64()
            .and_then(|v| DraftRevision::try_from(v).ok())
            .map(Some)
            .ok_or_else(refused),
        Some(_) => Err(refused()),
    }
}

/// Decode a body value into its REST DTO (`super::dto`), then convert it into the SDK request
/// type; either failure is `request-invalid`. A conversion failure names its static member; a
/// decode failure only its serde category (the serde message quotes the client's input).
fn decode<D, T>(operation: &'static str, value: Value) -> Result<T, OrdersError>
where
    D: serde::de::DeserializeOwned,
    T: TryFrom<D, Error = AuthoringInvalid>,
{
    let body: D = serde_json::from_value(value)
        .map_err(|error| refused_body(operation, None, Some(serde_category(&error))))?;
    T::try_from(body)
        .map_err(|AuthoringInvalid(member)| refused_body(operation, Some(member), None))
}

/// `{ "fields": { ... } }` exactly (plus the extracted revision).
fn fields(operation: &'static str, mut map: Map<String, Value>) -> Result<Value, OrdersError> {
    let refused = || refused_body(operation, Some("fields"), None);
    let fields = map.remove("fields").ok_or_else(refused)?;
    if !map.is_empty() {
        return Err(refused());
    }
    match &fields {
        Value::Object(named) if !named.is_empty() => Ok(fields),
        _ => Err(refused()),
    }
}

/// The `create` body: an object decoded as [`OrdersCreateOrder`].
///
/// # Errors
/// `request-invalid` for any body the wire refuses.
#[doc(hidden)]
pub fn create_body(body: &[u8]) -> Result<CreateOrder, OrdersError> {
    decode::<OrdersCreateOrder, _>("create", Value::Object(object("create", body, false)?))
}

/// The `patch_order` body: the extracted draft revision and the [`OrdersHeaderFields`] named
/// inside `{ "fields": { ... } }`.
///
/// # Errors
/// `request-invalid` for any body the wire refuses.
#[doc(hidden)]
pub fn patch_order_body(body: &[u8]) -> Result<(Option<DraftRevision>, HeaderPatch), OrdersError> {
    const OPERATION: &str = "patch_order";
    let mut map = object(OPERATION, body, false)?;
    let revision = take_draft_revision(OPERATION, &mut map)?;
    Ok((
        revision,
        decode::<OrdersHeaderFields, _>(OPERATION, fields(OPERATION, map)?)?,
    ))
}

/// The `add_line` body: the extracted draft revision and the rest decoded as [`OrdersAddLine`].
///
/// # Errors
/// `request-invalid` for any body the wire refuses.
#[doc(hidden)]
pub fn add_line_body(body: &[u8]) -> Result<(Option<DraftRevision>, AddLine), OrdersError> {
    const OPERATION: &str = "add_line";
    let mut map = object(OPERATION, body, false)?;
    let revision = take_draft_revision(OPERATION, &mut map)?;
    Ok((
        revision,
        decode::<OrdersAddLine, _>(OPERATION, Value::Object(map))?,
    ))
}

/// The `patch_line` body: the extracted draft revision and the [`OrdersLineFields`] named inside
/// `{ "fields": { ... } }`.
///
/// # Errors
/// `request-invalid` for any body the wire refuses.
#[doc(hidden)]
pub fn patch_line_body(body: &[u8]) -> Result<(Option<DraftRevision>, LinePatch), OrdersError> {
    const OPERATION: &str = "patch_line";
    let mut map = object(OPERATION, body, false)?;
    let revision = take_draft_revision(OPERATION, &mut map)?;
    Ok((
        revision,
        decode::<OrdersLineFields, _>(OPERATION, fields(OPERATION, map)?)?,
    ))
}

/// Existing-order write metadata: version, key, proof, then the body's draft revision.
fn write_meta(headers: &HeaderMap) -> Result<CallMeta, OrdersError> {
    let expected_version = expected_version(headers)?;
    let idempotency_key = idempotency_key(headers)?;
    let delegation_proof_ref = super::delegation_proof(headers)?;
    Ok(CallMeta {
        expected_version,
        idempotency_key,
        correlation_id: None,
        delegation_proof_ref,
    })
}

/// The stored response exactly as stored; a replay additionally carries
/// `Idempotency-Replayed: true` (DNA REST §8), a settled refusal included (ADR-0005, D-212).
fn render(settled: Settled) -> Response {
    let Settled {
        response, replayed, ..
    } = settled;
    let (status, body, stored_headers) = response.into_parts();
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let problem = !status.is_success();
    let mut out = (status, axum::Json(body)).into_response();
    let headers = out.headers_mut();
    for (name, value) in stored_headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::try_from(name.as_str()),
            HeaderValue::try_from(value.as_str()),
        ) {
            headers.insert(name, value);
        }
    }
    if problem {
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
    }
    if replayed {
        headers.insert(IDEMPOTENCY_REPLAYED, HeaderValue::from_static("true"));
    }
    out
}

/// `Location` of a created order, from the public request path: under a gateway `prefix_path`
/// the router is nested, and only the original URI still carries the prefix. Added at render
/// time, so a replay gets the same header as the first response. The order is the one the
/// engine recorded for the settlement, never read back out of the stored body.
fn with_location(mut settled: Settled, collection_path: &str) -> Settled {
    if settled.response.status() == StatusCode::CREATED.as_u16()
        && let Some(order_id) = settled.order_id
    {
        let location = format!("{}/{order_id}", collection_path.trim_end_matches('/'));
        if let Err(error) = settled.response.insert_location(location) {
            // Unreachable for an ASCII request path; the response is still the settled one.
            tracing::warn!(
                target: "orders.capture",
                order_id = %order_id,
                error = %error,
                "created-order Location not representable; rendered without it"
            );
        }
    }
    settled
}

async fn create_order(
    Extension(service): Extension<Arc<CaptureService>>,
    Extension(ctx): Extension<SecurityContext>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let idempotency_key = idempotency_key(&headers)?;
    let delegation_proof_ref = super::delegation_proof(&headers)?;
    let request = create_body(&body)?;
    let meta = CreateMeta {
        idempotency_key,
        correlation_id: None,
        delegation_proof_ref: delegation_proof_ref.clone(),
    };
    let caller = Caller::new(ctx, delegation_proof_ref);
    let settled = service.create(&caller, request, &meta).await?;
    Ok(render(with_location(settled, uri.path())))
}

async fn patch_order(
    Extension(service): Extension<Arc<CaptureService>>,
    Extension(limiter): Extension<Arc<PerOrderLimiter>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(order_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    limiter.admit(ctx.subject_id(), order_id)?;
    let call = write_meta(&headers)?;
    let (revision, patch) = patch_order_body(&body)?;
    let caller = Caller::from_meta(&ctx, call.delegation_proof_ref.as_ref())?;
    let meta = WriteMeta {
        call,
        expected_draft_revision: revision,
    };
    let settled = service.patch_order(&caller, order_id, patch, &meta).await?;
    Ok(render(settled))
}

async fn add_line(
    Extension(service): Extension<Arc<CaptureService>>,
    Extension(limiter): Extension<Arc<PerOrderLimiter>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(order_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    limiter.admit(ctx.subject_id(), order_id)?;
    let call = write_meta(&headers)?;
    let (revision, line) = add_line_body(&body)?;
    let caller = Caller::from_meta(&ctx, call.delegation_proof_ref.as_ref())?;
    let meta = WriteMeta {
        call,
        expected_draft_revision: revision,
    };
    let settled = service.add_line(&caller, order_id, line, &meta).await?;
    Ok(render(settled))
}

async fn patch_line(
    Extension(service): Extension<Arc<CaptureService>>,
    Extension(limiter): Extension<Arc<PerOrderLimiter>>,
    Extension(ctx): Extension<SecurityContext>,
    Path((order_id, line_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    limiter.admit(ctx.subject_id(), order_id)?;
    let call = write_meta(&headers)?;
    let (revision, patch) = patch_line_body(&body)?;
    let caller = Caller::from_meta(&ctx, call.delegation_proof_ref.as_ref())?;
    let meta = WriteMeta {
        call,
        expected_draft_revision: revision,
    };
    let settled = service
        .patch_line(&caller, order_id, line_id, patch, &meta)
        .await?;
    Ok(render(settled))
}

async fn remove_line(
    Extension(service): Extension<Arc<CaptureService>>,
    Extension(limiter): Extension<Arc<PerOrderLimiter>>,
    Extension(ctx): Extension<SecurityContext>,
    Path((order_id, line_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    limiter.admit(ctx.subject_id(), order_id)?;
    let call = write_meta(&headers)?;
    let mut map = object("remove_line", &body, true)?;
    let revision = take_draft_revision("remove_line", &mut map)?;
    if !map.is_empty() {
        return Err(refused_body("remove_line", None, Some("unknown_member")).into());
    }
    let caller = Caller::from_meta(&ctx, call.delegation_proof_ref.as_ref())?;
    let meta = WriteMeta {
        call,
        expected_draft_revision: revision,
    };
    let settled = service
        .remove_line(&caller, order_id, line_id, &meta)
        .await?;
    Ok(render(settled))
}

fn if_match() -> ParamSpec {
    ParamSpec::header("If-Match")
        .required(true)
        .description("Strong commercial-version ETag \"N\"; absent or malformed is 428")
}
fn key_param() -> ParamSpec {
    ParamSpec::header("Idempotency-Key")
        .required(true)
        .description("1-255 printable ASCII bytes, preserved exactly; same key replays the outcome")
}
fn etag() -> ResponseHeaderSpec {
    ResponseHeaderSpec::new(
        "ETag",
        "The current commercial version to send back as If-Match",
        ResponseHeaderType::String,
    )
}

/// Register the five authoring routes and attach the shared service and the edge limiter once
/// (`ToolKit` REST rule). Every route binds the gateway caller zone (D-185).
pub fn router(
    service: Arc<CaptureService>,
    limiter: Arc<PerOrderLimiter>,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    let router = Router::new();
    let router = OperationBuilder::post("/bss-orders/v1/orders")
        .operation_id("bss_orders.create")
        .summary("Create a draft order")
        .description(
            "Creates an empty draft at commercial version 1 and draft revision 0 with a \
             seller-unique order number. The actor and sales path come from the authenticated \
             context and the delegation proof header, never from the body. No catalog, contract, \
             price or total is resolved. Refusals: 400 REQUEST_INVALID; 403/404 per the shared \
             authorization contract; 409 CATEGORY_NOT_ADMITTED, IDEMPOTENCY_MISMATCH, \
             STILL_PROCESSING; 503 when authorization or the gear is unavailable.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .with_throttling(caller_write_throttling())
        .json_request::<OrdersCreateOrder>(openapi, "Draft header")
        .param(key_param())
        .param(super::delegation_proof_param())
        .handler(create_order)
        .json_response_with_schema::<OrdersOrderView>(openapi, StatusCode::CREATED, "Created draft")
        .response_header(etag())
        .standard_errors(openapi)
        .error_429(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::patch("/bss-orders/v1/orders/{orderId}")
        .operation_id("bss_orders.patch_order")
        .summary("Edit the order header")
        .description(
            "The named fields' classes alone select the trigger (no state is read to choose it): \
             any commercial or commercial-frozen field is a draft mutation; administrative fields \
             only are an administrative edit, which is not yet delivered (503). A request naming \
             both refuses MIXED_FIELD_CLASSES in draft; seller_tenant_id refuses \
             TENANT_AXIS_IMMUTABLE; outside draft a commercial edit refuses NOT_ADMISSIBLE first. \
             expected_draft_revision is optional here and compared after admissibility.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("orderId", "Order identity")
        .with_throttling(caller_write_throttling())
        .json_request::<OrdersHeaderPatch>(openapi, "Named header fields")
        .param(if_match())
        .param(key_param())
        .param(super::delegation_proof_param())
        .handler(patch_order)
        .json_response_with_schema::<OrdersTransitionResult>(openapi, StatusCode::OK, "Committed")
        .response_header(etag())
        .standard_errors(openapi)
        .problem_response(openapi, StatusCode::PRECONDITION_FAILED, STALE_IF_MATCH)
        .error_429(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-orders/v1/orders/{orderId}/lines")
        .operation_id("bss_orders.add_line")
        .summary("Add a draft line")
        .description(
            "Adds one working-set line with a server-reserved line_id, retaining dates, term and \
             cycle exactly as authored. Refusals include NOT_ADMISSIBLE outside draft, \
             VERSION_CONFLICT for a missing or stale expected_draft_revision, PRECONDITION_FAILED \
             (412) for a stale If-Match, CURRENCY_MIXED and \
             LINE_CAP_EXCEEDED.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("orderId", "Order identity")
        .with_throttling(caller_write_throttling())
        .json_request::<OrdersAddLine>(openapi, "Authored line")
        .param(if_match())
        .param(key_param())
        .param(super::delegation_proof_param())
        .handler(add_line)
        .json_response_with_schema::<OrdersTransitionResult>(openapi, StatusCode::OK, "Committed")
        .response_header(etag())
        .standard_errors(openapi)
        .problem_response(openapi, StatusCode::PRECONDITION_FAILED, STALE_IF_MATCH)
        .error_429(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::patch("/bss-orders/v1/orders/{orderId}/lines/{lineId}")
        .operation_id("bss_orders.patch_line")
        .summary("Edit a draft line")
        .description(
            "Commercial line fields are a draft mutation (draft only); administrative fields \
                 only are an administrative edit, not yet delivered (503). LINE_NOT_FOUND for a \
                 line that is not, or is no longer, a working-set member.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("orderId", "Order identity")
        .path_param("lineId", "Line identity")
        .with_throttling(caller_write_throttling())
        .json_request::<OrdersLinePatch>(openapi, "Named line fields")
        .param(if_match())
        .param(key_param())
        .param(super::delegation_proof_param())
        .handler(patch_line)
        .json_response_with_schema::<OrdersTransitionResult>(openapi, StatusCode::OK, "Committed")
        .response_header(etag())
        .standard_errors(openapi)
        .problem_response(openapi, StatusCode::PRECONDITION_FAILED, STALE_IF_MATCH)
        .error_429(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::delete("/bss-orders/v1/orders/{orderId}/lines/{lineId}")
        .operation_id("bss_orders.remove_line")
        .summary("Remove a draft line")
        .description(
            "Removes the line from the draft working set; its identity stays reserved and is \
                 never re-admitted. The optional JSON body carries only expected_draft_revision.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("orderId", "Order identity")
        .path_param("lineId", "Line identity")
        .with_throttling(caller_write_throttling())
        .json_request::<OrdersRemoveLine>(openapi, "Optional draft revision")
        .request_optional()
        .param(if_match())
        .param(key_param())
        .param(super::delegation_proof_param())
        .handler(remove_line)
        .json_response_with_schema::<OrdersTransitionResult>(openapi, StatusCode::OK, "Committed")
        .response_header(etag())
        .standard_errors(openapi)
        .problem_response(openapi, StatusCode::PRECONDITION_FAILED, STALE_IF_MATCH)
        .error_429(openapi)
        .error_503(openapi)
        .register(router, openapi);
    router.layer(Extension(service)).layer(Extension(limiter))
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
