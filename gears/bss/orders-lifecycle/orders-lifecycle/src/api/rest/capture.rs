//! The five draft authoring routes (DESIGN 02 §3.3; S1-02 catalog `create`, `patch_order`,
//! `add_line`, `patch_line`, `remove_line`).
//!
//! Boundary order (S1-02 `contract_checks.py`, CONTRACTS.md): required `If-Match` (428, nothing
//! else touched) → `Idempotency-Key` → `X-Delegation-Proof-Ref` → `expected_draft_revision` →
//! body shape. Each handler then enters the shared [`CaptureService`], which the local SDK uses
//! too. A settled outcome — success or registered refusal — is rendered exactly as stored, so a
//! same-key replay is byte-identical. Bodies are `snake_case` (D-206).
//!
//! Pre-engine throttling (S2-12, D-185): every route here binds the gateway's identity-keyed
//! caller zone, and the four existing-order writes consult the gear-local `(caller, order)`
//! limiter first, before any header, body, authorization or storage activity, so a throttled
//! attempt never reaches the engine or writes a row ([`super::throttle`]).
use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Router};
use bss_orders_lifecycle_sdk::OrdersError;
use bss_orders_lifecycle_sdk::authoring::{
    AddLine, CreateMeta, CreateOrder, HeaderPatch, LinePatch,
};
use bss_orders_lifecycle_sdk::catalog::Reason;
use bss_orders_lifecycle_sdk::models::{CallMeta, DraftRevision};
use serde_json::{Map, Value};
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{
    OperationBuilder, ParamSpec, ResponseHeaderSpec, ResponseHeaderType,
};
use toolkit::api::rest::extract::Path;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{
    OrdersAddLine, OrdersCreateOrder, OrdersHeaderPatch, OrdersLinePatch, OrdersOrderView,
    OrdersRemoveLine, OrdersTransitionResult,
};
use super::throttle::{PerOrderLimiter, caller_write_throttling, rejection_response};
use crate::domain::idempotency::StoredResponse;
use crate::infra::capture::{CaptureService, WriteMeta};

const IDEMPOTENCY_KEY: &str = "idempotency-key";
const IF_MATCH: &str = "if-match";
const EXPECTED_DRAFT_REVISION: &str = "expected_draft_revision";
const TAG: &str = "Orders Lifecycle";

fn invalid() -> OrdersError {
    OrdersError::Refused(Reason::RequestInvalid)
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
) -> Result<bss_orders_lifecycle_sdk::models::OrderVersion, OrdersError> {
    let value = single_header(headers, IF_MATCH)
        .map_err(|_| OrdersError::Refused(Reason::ExpectedVersionRequired))?;
    super::expected_version(value)
}

fn idempotency_key(
    headers: &HeaderMap,
) -> Result<bss_orders_lifecycle_sdk::models::IdempotencyKey, OrdersError> {
    super::idempotency_key(single_header(headers, IDEMPOTENCY_KEY)?)
}

/// The JSON object body; `allow_empty` admits an absent body (line removal).
fn object(body: &Bytes, allow_empty: bool) -> Result<Map<String, Value>, OrdersError> {
    if body.is_empty() && allow_empty {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(invalid()),
    }
}

/// Extract the optional independent `expected_draft_revision` member: a nonnegative signed bigint
/// (never a boolean, string or fraction).
fn take_draft_revision(map: &mut Map<String, Value>) -> Result<Option<DraftRevision>, OrdersError> {
    match map.remove(EXPECTED_DRAFT_REVISION) {
        None => Ok(None),
        Some(Value::Number(n)) => n
            .as_i64()
            .and_then(|v| DraftRevision::try_from(v).ok())
            .map(Some)
            .ok_or_else(invalid),
        Some(_) => Err(invalid()),
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, OrdersError> {
    serde_json::from_value(value).map_err(|_| invalid())
}

/// `{ "fields": { ... } }` exactly (plus the extracted revision).
fn fields(mut map: Map<String, Value>) -> Result<Value, OrdersError> {
    let fields = map.remove("fields").ok_or_else(invalid)?;
    if !map.is_empty() {
        return Err(invalid());
    }
    match &fields {
        Value::Object(named) if !named.is_empty() => Ok(fields),
        _ => Err(invalid()),
    }
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

fn render(response: StoredResponse) -> Response {
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let problem = !status.is_success();
    let mut out = (status, axum::Json(response.body)).into_response();
    let headers = out.headers_mut();
    for (name, value) in response.headers {
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
    out
}

fn refuse(error: OrdersError) -> Response {
    super::problem(error).into_response()
}

fn outcome(result: Result<StoredResponse, OrdersError>) -> Response {
    match result {
        Ok(response) => render(response),
        Err(error) => refuse(error),
    }
}

async fn create_order(
    Extension(service): Extension<Arc<CaptureService>>,
    Extension(ctx): Extension<SecurityContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let run = async {
        let idempotency_key = idempotency_key(&headers)?;
        let delegation_proof_ref = super::delegation_proof(&headers)?;
        let request: CreateOrder = decode(Value::Object(object(&body, false)?))?;
        let meta = CreateMeta {
            idempotency_key,
            correlation_id: None,
            delegation_proof_ref: delegation_proof_ref.clone(),
        };
        let caller = crate::authz::Caller::new(ctx, delegation_proof_ref);
        service.create(&caller, request, &meta).await
    };
    outcome(run.await)
}

async fn patch_order(
    Extension(service): Extension<Arc<CaptureService>>,
    Extension(limiter): Extension<Arc<PerOrderLimiter>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(order_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(rejection) = limiter.admit(ctx.subject_id(), order_id) {
        return rejection_response(rejection);
    }
    let run = async {
        let call = write_meta(&headers)?;
        let mut map = object(&body, false)?;
        let revision = take_draft_revision(&mut map)?;
        let patch: HeaderPatch = decode(fields(map)?)?;
        let caller = super::sdk_caller(&ctx, &call);
        let meta = WriteMeta {
            call,
            expected_draft_revision: revision,
        };
        service.patch_order(&caller, order_id, patch, &meta).await
    };
    outcome(run.await)
}

async fn add_line(
    Extension(service): Extension<Arc<CaptureService>>,
    Extension(limiter): Extension<Arc<PerOrderLimiter>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(order_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(rejection) = limiter.admit(ctx.subject_id(), order_id) {
        return rejection_response(rejection);
    }
    let run = async {
        let call = write_meta(&headers)?;
        let mut map = object(&body, false)?;
        let revision = take_draft_revision(&mut map)?;
        let line: AddLine = decode(Value::Object(map))?;
        let caller = super::sdk_caller(&ctx, &call);
        let meta = WriteMeta {
            call,
            expected_draft_revision: revision,
        };
        service.add_line(&caller, order_id, line, &meta).await
    };
    outcome(run.await)
}

async fn patch_line(
    Extension(service): Extension<Arc<CaptureService>>,
    Extension(limiter): Extension<Arc<PerOrderLimiter>>,
    Extension(ctx): Extension<SecurityContext>,
    Path((order_id, line_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(rejection) = limiter.admit(ctx.subject_id(), order_id) {
        return rejection_response(rejection);
    }
    let run = async {
        let call = write_meta(&headers)?;
        let mut map = object(&body, false)?;
        let revision = take_draft_revision(&mut map)?;
        let patch: LinePatch = decode(fields(map)?)?;
        let caller = super::sdk_caller(&ctx, &call);
        let meta = WriteMeta {
            call,
            expected_draft_revision: revision,
        };
        service
            .patch_line(&caller, order_id, line_id, patch, &meta)
            .await
    };
    outcome(run.await)
}

async fn remove_line(
    Extension(service): Extension<Arc<CaptureService>>,
    Extension(limiter): Extension<Arc<PerOrderLimiter>>,
    Extension(ctx): Extension<SecurityContext>,
    Path((order_id, line_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(rejection) = limiter.admit(ctx.subject_id(), order_id) {
        return rejection_response(rejection);
    }
    let run = async {
        let call = write_meta(&headers)?;
        let mut map = object(&body, true)?;
        let revision = take_draft_revision(&mut map)?;
        if !map.is_empty() {
            return Err(invalid());
        }
        let caller = super::sdk_caller(&ctx, &call);
        let meta = WriteMeta {
            call,
            expected_draft_revision: revision,
        };
        service.remove_line(&caller, order_id, line_id, &meta).await
    };
    outcome(run.await)
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
#[allow(
    clippy::too_many_lines,
    reason = "one OperationBuilder chain per route keeps every route's contract in one place"
)]
pub fn router(
    service: Arc<CaptureService>,
    limiter: Arc<PerOrderLimiter>,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    let router = Router::new();
    let router = OperationBuilder::post("/bss-orders-lifecycle/v1/orders")
        .operation_id("bss_orders_lifecycle.create")
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
    let router = OperationBuilder::patch("/bss-orders-lifecycle/v1/orders/{orderId}")
        .operation_id("bss_orders_lifecycle.patch_order")
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
        .error_429(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-orders-lifecycle/v1/orders/{orderId}/lines")
        .operation_id("bss_orders_lifecycle.add_line")
        .summary("Add a draft line")
        .description(
            "Adds one working-set line with a server-reserved line_id, retaining dates, term and \
             cycle exactly as authored. Refusals include NOT_ADMISSIBLE outside draft, \
             VERSION_CONFLICT for a missing or stale expected_draft_revision, CURRENCY_MIXED and \
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
        .error_429(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router =
        OperationBuilder::patch("/bss-orders-lifecycle/v1/orders/{orderId}/lines/{lineId}")
            .operation_id("bss_orders_lifecycle.patch_line")
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
            .json_response_with_schema::<OrdersTransitionResult>(
                openapi,
                StatusCode::OK,
                "Committed",
            )
            .response_header(etag())
            .standard_errors(openapi)
            .error_429(openapi)
            .error_503(openapi)
            .register(router, openapi);
    let router =
        OperationBuilder::delete("/bss-orders-lifecycle/v1/orders/{orderId}/lines/{lineId}")
            .operation_id("bss_orders_lifecycle.remove_line")
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
            .json_response_with_schema::<OrdersTransitionResult>(
                openapi,
                StatusCode::OK,
                "Committed",
            )
            .response_header(etag())
            .standard_errors(openapi)
            .error_429(openapi)
            .error_503(openapi)
            .register(router, openapi);
    router.layer(Extension(service)).layer(Extension(limiter))
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
