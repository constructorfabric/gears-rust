//! REST handlers for the usage-collector gateway.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use http::StatusCode;
use modkit::api::problem::Problem;
use modkit_odata::{ODataOrderBy, validate_cursor_against};
use modkit_security::SecurityContext;
use usage_collector_sdk::models::GroupByDimension;
use usage_collector_sdk::{
    CursorV1, RawQueryFilters, SortDir, UsageCollectorError, raw_query_effective_order,
    raw_query_filter_hash,
};
use usage_emitter::UsageEmitterRuntimeV1;
use uuid::Uuid;

use crate::domain::{AggregationQueryRequest, DomainError, RawQueryRequest, Service};

// @cpt-begin:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-0
/// Default number of raw records returned per page when `page_size` is absent.
///
/// Used by [`handle_query_raw`] when the request omits `page_size`
/// (`inst-raw-3`).
pub(crate) const DEFAULT_PAGE_SIZE: u32 = 100;

/// Maximum allowed value for `page_size` in raw queries.
///
/// Validated against `inst-raw-3` (`page_size ∈ [1, MAX_PAGE_SIZE]`); requests
/// outside this range return `400 Bad Request`.
pub(crate) const MAX_PAGE_SIZE: u32 = 1_000;

/// Maximum byte length for string filter fields (`usage_type`, `resource_type`,
/// `subject_type`, `source`).
///
/// Enforced by both query handlers (`inst-agg-3`, `inst-raw-3`); requests with
/// any string filter exceeding this length return `400 Bad Request`.
pub(crate) const MAX_FILTER_STRING_LEN: usize = 256;

/// Maximum allowed query time range `(from, to)` per request (~1 year).
///
/// Enforced by both query handlers (`inst-agg-3b`, `inst-raw-3b`); requests
/// whose `(to - from)` exceeds this duration return `400 Bad Request`.
#[allow(clippy::duration_suboptimal_units)] // `Duration::from_days` is not stable as `const fn`
pub(crate) const MAX_QUERY_TIME_RANGE: Duration = Duration::from_secs(366 * 24 * 60 * 60);
// @cpt-end:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-0

/// Cursor-failure strings surfaced in the 400 `details` array by
/// [`decode_and_validate_cursor`]. Pulled out as named constants so a wording
/// change is a one-line edit instead of a co-ordinated rename across the
/// handler and the tests that pin each failure mode.
pub(crate) const CURSOR_ERR_DECODE_FAILED: &str = "cursor: decode failed";
pub(crate) const CURSOR_ERR_UNSUPPORTED_SORT: &str = "cursor: unsupported sort direction";
pub(crate) const CURSOR_ERR_UNSUPPORTED_PAGINATION: &str =
    "cursor: unsupported pagination direction";
pub(crate) const CURSOR_ERR_INVALID_KEY_SHAPE: &str = "cursor: invalid key shape";
pub(crate) const CURSOR_ERR_INVALID_TIMESTAMP: &str = "cursor: invalid timestamp";
pub(crate) const CURSOR_ERR_INVALID_ID: &str = "cursor: invalid id";
pub(crate) const CURSOR_ERR_OUTSIDE_RANGE: &str =
    "cursor: timestamp is outside the requested [from, to] range";
/// Cursor's signed sort tokens (`cursor.s`) do not match the gateway's
/// effective order for this endpoint (`+timestamp,+id`). Surfaced when a
/// caller pastes a cursor minted under a different sort signature.
pub(crate) const CURSOR_ERR_ORDER_MISMATCH: &str = "cursor: invalid for this query (sort mismatch)";
/// Cursor's filter hash (`cursor.f`) does not match the request's effective
/// filter hash. Only fires when the cursor was minted with a filter hash —
/// cursors minted with `f = None` skip this check (per
/// [`modkit_odata::validate_cursor_against`]).
pub(crate) const CURSOR_ERR_FILTER_MISMATCH: &str =
    "cursor: invalid for this query (filter mismatch)";
/// Generic fallback for any future `modkit_odata::Error` variant that
/// `validate_cursor_against` may grow beyond the order/filter mismatch pair
/// it returns today. Surfacing a neutral "invalid for this query" string is
/// preferable to aliasing the new variant to a specific mismatch reason —
/// the latter would mislead operators chasing a deploy-time cursor breakage.
pub(crate) const CURSOR_ERR_INVALID: &str = "cursor: invalid for this query";

use super::dto::{
    AggregatedQueryParams, AggregationResultDto, AllowedMetricResponse, CreateUsageRecordRequest,
    ModuleConfigResponse, RawQueryParams, RawQueryResponse,
};

/// Handler for `POST /usage-collector/v1/records`.
///
/// # Errors
/// Returns a [`Problem`] on authorization failure, validation errors, or internal emitter failure.
#[tracing::instrument(
    skip_all,
    fields(
        module = %req.module,
        tenant_id = %req.tenant_id,
        resource_id = %req.resource_id,
    ),
)]
pub async fn handle_create_usage_record(
    Extension(ctx): Extension<SecurityContext>,
    Extension(runtime): Extension<Arc<dyn UsageEmitterRuntimeV1>>,
    Json(req): Json<CreateUsageRecordRequest>,
) -> Result<StatusCode, Problem> {
    let emitter = authorize_request(&ctx, &runtime, &req).await?;

    let mut builder = emitter
        .usage_record_builder(req.metric, req.value)
        .map_err(|e| canonical_error_to_problem(&e))?
        .with_timestamp(req.timestamp);

    // Normalize whitespace-only keys at the boundary so the builder always sees
    // `Some(non-empty)` or `None`. A raw whitespace key is treated as missing for gauges
    // (UUID generated at enqueue time) and surfaces as the canonical InvalidArgument for
    // counters at validation — consistent across both metric kinds.
    if let Some(key) = req
        .idempotency_key
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
    {
        builder = builder.with_idempotency_key(key);
    }

    if let Some(meta) = req.metadata {
        builder = builder.with_metadata(meta);
    }

    // `usage_record_builder` prefills every required field (module, tenant_id,
    // resource_id, resource_type, metric, kind, value); the optional setters above
    // only add fields, never clear required ones — so this `.build()` is currently
    // unreachable on the error branch. The prefill invariant is regression-tested
    // by `cyberware-usage-emitter`'s `usage_record_builder_prefills_authorized_fields_for_gauge`
    // and `usage_record_builder_resolves_counter_kind_from_allowed_metrics` (in
    // `usage_record_builder_tests.rs`), which assert that `.build()` succeeds after
    // the prefill alone. We still funnel any future `.build()` error through
    // `canonical_error_to_problem` so a legitimate failure path keeps the canonical
    // 4xx contract instead of panicking inside an axum handler.
    let record = builder
        .build()
        .map_err(|e| canonical_error_to_problem(&e))?;
    emitter
        .enqueue(record)
        .await
        .map_err(|e| canonical_error_to_problem(&e))?;

    // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-gateway-ingest-handler:inst-gw-7
    Ok(StatusCode::NO_CONTENT)
    // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-gateway-ingest-handler:inst-gw-7
}

async fn authorize_request(
    ctx: &SecurityContext,
    runtime: &Arc<dyn UsageEmitterRuntimeV1>,
    req: &CreateUsageRecordRequest,
) -> Result<usage_emitter::UsageEmitter, Problem> {
    // Forward the caller's stated subject intent verbatim. `Some(s)` becomes
    // an explicit binding; `None` becomes the explicit "no subject" choice —
    // NOT the default-from-context fallback. Substituting the gateway's own
    // `SecurityContext` subject when the caller sent `None` would silently
    // attribute the request to the wrong principal at the PDP boundary.
    let factory = runtime.factory(&req.module).with_tenant(req.tenant_id);
    let factory = match req.subject.clone() {
        Some(s) => factory.with_subject(s),
        None => factory.without_subject(),
    };
    factory
        .authorize(ctx, req.resource_id, &req.resource_type)
        .await
        .map_err(|e| canonical_error_to_problem(&e))
}

/// Handler for `GET /usage-collector/v1/modules/{module_name}/config`.
///
/// # Errors
/// Returns a [`Problem`] if the module is not found or the collector call fails.
// @cpt-flow:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2
#[tracing::instrument(skip_all, fields(module = %module_name))]
pub async fn handle_get_module_config(
    Path(module_name): Path<String>,
    Extension(service): Extension<Arc<Service>>,
) -> Result<Json<ModuleConfigResponse>, Problem> {
    // @cpt-begin:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-2
    // Authenticated request received; ModKit pipeline enforces authentication before handler entry.
    // @cpt-end:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-2

    // @cpt-begin:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-3
    // Convert DomainError → UsageCollectorError via From, then funnel through the canonical
    // problem mapping so REST has a single source of truth for HTTP status / detail.
    let config = service
        .get_module_config(&module_name)
        .map_err(|e| canonical_error_to_problem(&UsageCollectorError::from(e)))?;
    // @cpt-end:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-3

    // @cpt-begin:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-5
    let response = ModuleConfigResponse {
        allowed_metrics: config
            .allowed_metrics
            .into_iter()
            .map(|m| AllowedMetricResponse {
                name: m.name,
                kind: m.kind,
            })
            .collect(),
        max_metadata_bytes: config.max_metadata_bytes,
    };

    Ok(Json(response))
    // @cpt-end:cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config:p2:inst-cfg-5
}

/// Handler for `GET /usage-collector/v1/aggregated`.
///
/// Validates request parameters per CDSL `inst-agg-3`/`-3a`/`-3b`/`-4`, derives
/// tenant from [`SecurityContext`] (never from a query parameter — the DTO has
/// no `tenant_id` field), and delegates to [`Service::query_aggregated`] which
/// performs the PDP authorization and embeds the compiled `AccessScope` into
/// the query before calling the storage plugin.
///
/// # Errors
///
/// All non-2xx responses are RFC 9457 `application/problem+json` documents; the
/// shapes shown below are the `context` extension member, not a top-level body.
///
/// - `400 Bad Request` with `context = {"error":"validation failed","code":"VALIDATION_ERROR","details":[..]}`
///   on validation failures (`inst-agg-4a`).
/// - `403 Forbidden` with `context = {"error":"forbidden"}` (no PDP details leaked)
///   when the PDP denies the request or returns a non-Denied error (`inst-agg-6a`).
/// - `503 Service Unavailable` with `context = {"error":"service_unavailable","correlation_id":"<id>"}`,
///   emitted by the canonical Problem mapper from
///   `UsageCollectorError::ServiceUnavailable`, on any other plugin error path
///   (`inst-agg-8c`). The handler emits an `ERROR` log with the same
///   `correlation_id` before returning.
// @cpt-flow:cpt-cf-usage-collector-flow-query-api-aggregated:p1
#[tracing::instrument(skip_all, fields(subject_id = %ctx.subject_id()))]
pub async fn handle_query_aggregated(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Query(params): Query<AggregatedQueryParams>,
) -> Result<Json<Vec<AggregationResultDto>>, Problem> {
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-1
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-2
    // tenant_id is derived from `ctx` at the PDP boundary via Service::query_aggregated;
    // the DTO has no `tenant_id` field, so the request cannot supply one.
    let mut errors: Vec<String> = Vec::new();
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-2
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-1

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3a
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3b
    validate_time_range(params.from, params.to, &mut errors);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3b
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3a

    if params
        .group_by
        .iter()
        .any(|d| matches!(d, GroupByDimension::TimeBucket(_)))
        && params.bucket_size.is_none()
    {
        errors.push("bucket_size: required when group_by includes time_bucket".to_owned());
    }

    push_filter_length_error(params.usage_type.as_ref(), "usage_type", &mut errors);
    push_filter_length_error(params.resource_type.as_ref(), "resource_type", &mut errors);
    push_filter_length_error(params.subject_type.as_ref(), "subject_type", &mut errors);
    push_filter_length_error(params.source.as_ref(), "source", &mut errors);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-4
    if !errors.is_empty() {
        // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-4a
        return Err(validation_problem(&errors));
        // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-4a
    }
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-4

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-7
    // `scope` is compiled by `Service::query_aggregated` from the PDP response
    // and embedded via `AggregationQueryRequest::with_scope` before the plugin
    // call; the request type intentionally has no `scope` field so a default /
    // un-compiled scope cannot be expressed at this boundary.
    let request = AggregationQueryRequest {
        time_range: (params.from, params.to),
        function: params.fn_,
        group_by: params.group_by,
        bucket_size: params.bucket_size,
        usage_type: params.usage_type,
        resource_id: params.resource_id,
        resource_type: params.resource_type,
        subject_id: params.subject_id,
        subject_type: params.subject_type,
        source: params.source,
    };
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-7

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-5
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-6
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-8
    let rows = match service.query_aggregated(&ctx, request).await {
        Ok(rows) => rows,
        // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-6a
        Err(e) if is_permission_denied(&e) => return Err(forbidden_problem()),
        // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-6a
        // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-8c
        Err(e) => return Err(service_unavailable_problem(&e, "query_aggregated")),
        // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-8c
    };
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-8
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-6
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-5

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-9
    let response: Vec<AggregationResultDto> =
        rows.into_iter().map(AggregationResultDto::from).collect();
    Ok(Json(response))
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-9
}

/// Handler for `GET /usage-collector/v1/raw`.
///
/// Validates request parameters per CDSL `inst-raw-3`/`-3a`/`-3b`/`-4`, derives
/// tenant from [`SecurityContext`], decodes the optional [`CursorV1`] and
/// confirms its timestamp lies in `[from, to]`, then delegates to
/// [`Service::query_raw`].
///
/// # Errors
///
/// Same shape as [`handle_query_aggregated`] — non-2xx responses are RFC 9457
/// `application/problem+json` documents and the `{"error":...}` envelope sits
/// inside the `context` extension member:
/// - `400` on validation failure or cursor decode failure (`inst-raw-4a`),
/// - `403` on PDP denial (`inst-raw-6a`, `context = {"error":"forbidden"}`),
/// - `503` on any other plugin error via the canonical Problem mapper
///   (`inst-raw-8b`).
// @cpt-flow:cpt-cf-usage-collector-flow-query-api-raw:p2
#[tracing::instrument(skip_all, fields(subject_id = %ctx.subject_id()))]
pub async fn handle_query_raw(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Query(params): Query<RawQueryParams>,
) -> Result<Json<RawQueryResponse>, Problem> {
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-1
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-2
    // tenant_id is derived from `ctx` at the PDP boundary via Service::query_raw;
    // the DTO has no `tenant_id` field, so the request cannot supply one.
    let mut errors: Vec<String> = Vec::new();
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-2
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-1

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3a
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3b
    validate_time_range(params.from, params.to, &mut errors);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3b
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3a

    // `validate_page_size` always returns a `u32`; on the error path it pushes
    // an error onto `errors` and returns `DEFAULT_PAGE_SIZE`. The `!errors.is_empty()`
    // guard below short-circuits before the value is used, so the default is
    // never forwarded to the plugin on the error path.
    let page_size = validate_page_size(params.page_size, &mut errors);

    push_filter_length_error(params.usage_type.as_ref(), "usage_type", &mut errors);
    push_filter_length_error(params.resource_type.as_ref(), "resource_type", &mut errors);
    push_filter_length_error(params.subject_type.as_ref(), "subject_type", &mut errors);

    // `effective_order` / `effective_filter_hash` are only consumed when a
    // cursor is actually being validated; computing them eagerly would run
    // the SHA-256 for every page-1 request that doesn't ship a `cursor`
    // query parameter. Bind them inside the closure so they materialize
    // only on the cursor-supplied path.
    let decoded_cursor = params.cursor.as_deref().and_then(|cursor_str| {
        let effective_filter_hash = raw_query_filter_hash(&RawQueryFilters {
            from: params.from,
            to: params.to,
            usage_type: params.usage_type.as_deref(),
            resource_id: params.resource_id,
            resource_type: params.resource_type.as_deref(),
            subject_type: params.subject_type.as_deref(),
            subject_id: params.subject_id,
        });
        decode_and_validate_cursor(
            cursor_str,
            params.from,
            params.to,
            raw_query_effective_order(),
            Some(effective_filter_hash.as_str()),
            &mut errors,
        )
    });
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-4
    if !errors.is_empty() {
        // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-4a
        return Err(validation_problem(&errors));
        // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-4a
    }
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-4

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-7
    // `scope` is compiled and embedded by `Service::query_raw` via
    // `RawQueryRequest::with_scope`; the request type carries no `scope` field
    // so a placeholder cannot leak to the plugin.
    let request = RawQueryRequest {
        time_range: (params.from, params.to),
        usage_type: params.usage_type,
        resource_id: params.resource_id,
        resource_type: params.resource_type,
        subject_type: params.subject_type,
        subject_id: params.subject_id,
        cursor: decoded_cursor,
        page_size,
    };
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-7

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-5
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-6
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-8
    let paged = match service.query_raw(&ctx, request).await {
        Ok(p) => p,
        // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-6a
        Err(e) if is_permission_denied(&e) => return Err(forbidden_problem()),
        // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-6a
        // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-8b
        Err(e) => return Err(service_unavailable_problem(&e, "query_raw")),
        // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-8b
    };
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-8
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-6
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-5

    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-9
    Ok(Json(RawQueryResponse::from(paged)))
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-9
}

/// Validate the request's `[from, to]` window in one place.
///
/// Branches on the sign of `to - from` directly so the upper-bound check is not
/// silently dead when `to <= from` — accumulates `from >= to` and `range too
/// wide` as independent violations so callers see every problem at once.
fn validate_time_range(from: DateTime<Utc>, to: DateTime<Utc>, errors: &mut Vec<String>) {
    let delta = to.signed_duration_since(from);
    if delta <= chrono::Duration::zero() {
        errors.push("time range must be strictly ascending (from must be before to)".to_owned());
        return;
    }
    if let Ok(d) = delta.to_std()
        && d > MAX_QUERY_TIME_RANGE
    {
        errors.push("time range exceeds maximum allowed duration".to_owned());
    }
}

/// Validate `page_size` against `inst-raw-3` (`page_size ∈ [1, MAX_PAGE_SIZE]`).
///
/// Always returns a `u32`: the validated value on the happy path, or
/// `DEFAULT_PAGE_SIZE` after pushing an error onto `errors`. The call site's
/// `!errors.is_empty()` guard short-circuits before the returned value is
/// forwarded to the plugin, so the default never leaks past the 400 boundary
/// on the error path.
fn validate_page_size(value: Option<u32>, errors: &mut Vec<String>) -> u32 {
    match value {
        None => DEFAULT_PAGE_SIZE,
        Some(0) => {
            errors.push("page_size: must be at least 1".to_owned());
            DEFAULT_PAGE_SIZE
        }
        Some(ps) if ps > MAX_PAGE_SIZE => {
            errors.push(format!("page_size: must not exceed {MAX_PAGE_SIZE}"));
            DEFAULT_PAGE_SIZE
        }
        Some(ps) => ps,
    }
}

fn push_filter_length_error(field: Option<&String>, name: &str, errors: &mut Vec<String>) {
    if let Some(s) = field
        && s.len() > MAX_FILTER_STRING_LEN
    {
        errors.push(format!(
            "{name}: exceeds maximum length of {MAX_FILTER_STRING_LEN} bytes"
        ));
    }
}

fn validation_problem(errors: &[String]) -> Problem {
    // The structured envelope (`error`, `code`, `details`) lands as an
    // RFC 9457 extension on `context` so clients consume one JSON document
    // instead of `JSON.parse(body.detail)`. `detail` carries a plain human
    // sentence; the wire-contract field names live in `context`.
    Problem::new(
        StatusCode::BAD_REQUEST,
        "Validation error",
        "One or more request parameters failed validation.",
    )
    .with_context(serde_json::json!({
        "error": "validation failed",
        "code": "VALIDATION_ERROR",
        "details": errors,
    }))
}

fn forbidden_problem() -> Problem {
    // FEATURE 0003 DoD §5: no PDP details, constraint names, policy names, or
    // role names leaked. The `{"error":"forbidden"}` envelope is attached as
    // RFC 9457 `context` so clients parse a single JSON document rather than
    // a JSON string embedded in `detail`.
    //
    // No `correlation_id` is emitted in the 403 body (paralleling the 503
    // path) and no explicit log line is emitted here — operator triage of a
    // 403 deny burst relies entirely on the parent span's
    // `#[tracing::instrument(skip_all, fields(subject_id = %ctx.subject_id()))]`
    // annotation on `handle_query_aggregated` / `handle_query_raw`. A
    // subscriber that flattens span fields onto exit events will capture the
    // subject_id, so dedicated dashboards / alerts on PDP denials should be
    // built off the span-exit signal rather than a request-body identifier.
    Problem::new(
        StatusCode::FORBIDDEN,
        "Forbidden",
        "Request denied by policy.",
    )
    .with_context(serde_json::json!({ "error": "forbidden" }))
}

/// Collapse any non-PermissionDenied query error into a single
/// `503 Service Unavailable` envelope.
///
/// This intentionally does NOT route through `canonical_error_to_problem(&e)`:
/// per the planning decision pinned by
/// `handle_query_aggregated_resource_exhausted_routes_through_canonical_503_not_400`,
/// the gateway exposes a uniform 503 contract for every non-Denied plugin
/// failure (`Timeout`, `CircuitOpen`, `Plugin(InvalidArgument | ResourceExhausted | …)`,
/// `PluginNotFound`, `PluginUnavailable`, `Internal`, …) so clients do not
/// have to discriminate plugin-specific status codes. The trade-off is that
/// per-variant canonical context (quota violations, resource info, etc.)
/// does NOT appear in the response body; the variant tag is preserved
/// server-side via the `error_variant` field on the ERROR log line below,
/// which is the operator-triage channel paired with `correlation_id` in the
/// 503 body (`inst-agg-8c` / `inst-raw-8b`).
///
/// `correlation_id` is a fresh per-request UUID so operators can map one
/// specific 503 response to one specific ERROR log line; reusing
/// `SecurityContext::subject_id` would collapse all 503s from a single caller
/// into a single id.
fn service_unavailable_problem(e: &DomainError, op: &'static str) -> Problem {
    let correlation_id = Uuid::new_v4().to_string();
    // Log only the stable alphanumeric variant tag plus the canonical HTTP
    // status code the inner error would map to. The previous `error_reason`
    // field surfaced `DomainError::Plugin(canonical).detail()`, but that
    // string is set by whichever storage plugin produced the error (an
    // out-of-tree third-party crate may pass SQL fragments, internal
    // identifiers, or raw row data via `.with_detail(...)`). The variant tag
    // partitions failures by `DomainError` arm; `canonical_status_code`
    // partitions the `Plugin(_)` arm further (`InvalidArgument` vs.
    // `ResourceExhausted` vs. `Internal` vs. `ServiceUnavailable`, …) so
    // operators can triage by status code without relying on plugin-supplied
    // strings landing in long-retention observability storage.
    //
    // `subject_id` is intentionally NOT re-emitted here: the parent span
    // (`handle_query_aggregated` / `handle_query_raw`) is annotated with
    // `#[tracing::instrument(skip_all, fields(subject_id = %ctx.subject_id()))]`,
    // so a subscriber that flattens span + event fields would otherwise emit
    // `subject_id` twice in the same record.
    tracing::error!(
        correlation_id = %correlation_id,
        error_variant = domain_error_variant(e),
        canonical_status_code = domain_error_canonical_status_code(e),
        "Storage plugin error during {op}"
    );
    // Build a canonical `ServiceUnavailable` so the Problem inherits the
    // canonical mapper's type URL and title; attach the wire envelope
    // (`error`, `correlation_id`) as RFC 9457 `context` rather than embedding
    // a JSON-encoded string in `detail`. Downstream tests can read
    // `correlation_id` from either the log or the response context.
    let canonical = UsageCollectorError::service_unavailable()
        .with_detail("Storage plugin is currently unavailable.")
        .create();
    canonical_error_to_problem(&canonical).with_context(serde_json::json!({
        "error": "service_unavailable",
        "correlation_id": correlation_id,
    }))
}

fn is_permission_denied(e: &DomainError) -> bool {
    matches!(e, DomainError::PermissionDenied(_))
}

/// Stable, alphanumeric variant tag for a [`DomainError`], suitable for
/// structured logging. Whitelisted to ensure no plugin-supplied free-form
/// strings (vendor names, gts ids, reason messages) end up in the log line.
///
/// The `PermissionDenied` arm is defense-in-depth: the only caller is
/// [`service_unavailable_problem`], which is only entered after the
/// `is_permission_denied(&e)` guard short-circuits to `forbidden_problem()`
/// at the handler call sites. The arm is kept (rather than collapsed under
/// `_`) so the `match` stays exhaustive over `DomainError` — adding a new
/// variant is a compile-time error rather than a silent fallthrough.
fn domain_error_variant(e: &DomainError) -> &'static str {
    match e {
        DomainError::TypesRegistryUnavailable(_) => "types_registry_unavailable",
        DomainError::ClientHub(_) => "client_hub",
        DomainError::PluginNotFound { .. } => "plugin_not_found",
        DomainError::InvalidPluginInstance { .. } => "invalid_plugin_instance",
        DomainError::PluginUnavailable { .. } => "plugin_unavailable",
        DomainError::Timeout => "timeout",
        DomainError::CircuitOpen => "circuit_open",
        DomainError::ModuleNotConfigured { .. } => "module_not_configured",
        DomainError::Plugin(_) => "plugin",
        DomainError::PermissionDenied(_) => "permission_denied",
        DomainError::Internal(_) => "internal",
    }
}

/// Canonical HTTP status code that the inner error would map to via
/// `From<DomainError> for UsageCollectorError`, exposed as a second structured
/// log field so operators can partition the `Plugin(_)` arm by the inner
/// canonical variant (`InvalidArgument` → 400, `ResourceExhausted` → 429,
/// `Internal` → 500, `ServiceUnavailable` → 503, …) without inspecting any
/// plugin-supplied free-form strings.
///
/// Status codes are stable, operator-controlled `u16`s — safe to surface
/// alongside [`domain_error_variant`] in long-retention observability storage.
/// Mirrors the mapping in [`crate::domain::error`] so the two stay in lockstep.
fn domain_error_canonical_status_code(e: &DomainError) -> u16 {
    match e {
        DomainError::Plugin(canonical) | DomainError::PermissionDenied(canonical) => {
            canonical.status_code()
        }
        DomainError::ModuleNotConfigured { .. } => 404,
        DomainError::Timeout => 504,
        DomainError::CircuitOpen
        | DomainError::PluginNotFound { .. }
        | DomainError::PluginUnavailable { .. }
        | DomainError::TypesRegistryUnavailable(_) => 503,
        DomainError::InvalidPluginInstance { .. }
        | DomainError::ClientHub(_)
        | DomainError::Internal(_) => 500,
    }
}

fn decode_and_validate_cursor(
    cursor_str: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    effective_order: &ODataOrderBy,
    effective_filter_hash: Option<&str>,
    errors: &mut Vec<String>,
) -> Option<CursorV1> {
    // Surface the failure modes as distinct strings so an operator staring at
    // a 400 response can tell a stale-but-well-formed token apart from a
    // structurally-broken one. The strings are deliberately coarse — they
    // describe the failure class, not the internal cursor layout — so the
    // wire contract still does not leak base64/CBOR encoding details.
    let Ok(cursor) = CursorV1::decode(cursor_str) else {
        errors.push(CURSOR_ERR_DECODE_FAILED.to_owned());
        return None;
    };
    // Pin the cursor shape the gateway issues for this endpoint: ascending
    // forward keyset on `(timestamp, id)`. A cursor minted by another endpoint,
    // a future schema, or hand-rolled against a different sort signature is
    // rejected here as defense-in-depth before it reaches the plugin.
    if cursor.o != SortDir::Asc {
        errors.push(CURSOR_ERR_UNSUPPORTED_SORT.to_owned());
        return None;
    }
    if cursor.d != "fwd" {
        errors.push(CURSOR_ERR_UNSUPPORTED_PAGINATION.to_owned());
        return None;
    }
    if cursor.k.len() < 2 {
        errors.push(CURSOR_ERR_INVALID_KEY_SHAPE.to_owned());
        return None;
    }
    let ts_str = cursor.k[0].as_str();
    let Ok(ts) = DateTime::parse_from_rfc3339(ts_str) else {
        errors.push(CURSOR_ERR_INVALID_TIMESTAMP.to_owned());
        return None;
    };
    if Uuid::parse_str(cursor.k[1].as_str()).is_err() {
        errors.push(CURSOR_ERR_INVALID_ID.to_owned());
        return None;
    }
    let ts_utc = ts.with_timezone(&Utc);
    if ts_utc < from || ts_utc > to {
        errors.push(CURSOR_ERR_OUTSIDE_RANGE.to_owned());
        return None;
    }
    // Final consistency check against the gateway's effective sort signature
    // and filter hash. `cursor.o == Asc` only proves the cursor's primary
    // direction; `cursor.s` is the full signed-token list (e.g.
    // `"+timestamp,+id"`) and must agree with the effective order so a cursor
    // minted under a different sort signature cannot be replayed here. The
    // filter-hash arm is opt-in: it only fires when both sides set the hash
    // (see `modkit_odata::validate_cursor_against`), so cursors minted with
    // `f = None` are still accepted — but a cursor stamped with a stale
    // filter hash is rejected before it reaches the plugin.
    if let Err(e) = validate_cursor_against(&cursor, effective_order, effective_filter_hash) {
        push_cursor_validate_error(&cursor, effective_order, effective_filter_hash, &e, errors);
        return None;
    }
    Some(cursor)
}

/// Translate a `modkit_odata::Error` from `validate_cursor_against` into the
/// gateway's user-facing cursor error string, and emit a structured
/// `tracing::debug` line so operators chasing a deploy-time cursor breakage
/// can grep the log rather than decoding the 400 body. Split out of
/// [`decode_and_validate_cursor`] so that function stays below the workspace
/// clippy cognitive-complexity threshold.
///
/// `validate_cursor_against` only returns `OrderMismatch` / `FilterMismatch`
/// today, but `modkit_odata::Error` is a wider enum and the function may
/// grow new failure modes. Surface those as a neutral "invalid for this
/// query" rather than aliasing them to a specific mismatch reason (which
/// would mislead debugging) or leaking the variant name to operators.
fn push_cursor_validate_error(
    cursor: &CursorV1,
    effective_order: &ODataOrderBy,
    effective_filter_hash: Option<&str>,
    err: &modkit_odata::Error,
    errors: &mut Vec<String>,
) {
    let (reason, err_string) = match err {
        modkit_odata::Error::OrderMismatch => {
            ("sort signature mismatch", CURSOR_ERR_ORDER_MISMATCH)
        }
        modkit_odata::Error::FilterMismatch => ("filter hash mismatch", CURSOR_ERR_FILTER_MISMATCH),
        _ => ("unrecognized validation error", CURSOR_ERR_INVALID),
    };
    tracing::debug!(
        cursor_s = %cursor.s,
        cursor_f = ?cursor.f,
        effective_s = %effective_order.to_signed_tokens(),
        effective_f = ?effective_filter_hash,
        error = %err,
        reason,
        "cursor rejected",
    );
    errors.push(err_string.to_owned());
}

fn canonical_error_to_problem(e: &UsageCollectorError) -> Problem {
    // Round-trip through the canonical Problem so its serialized per-category
    // context (reason / constraint / violations / resource_type / resource_name)
    // is preserved as an RFC 9457 extension member rather than dropped to a
    // title/detail pair. Built-in canonical contexts are plain structs and
    // serialize infallibly; a `from_error` failure is a Serialize regression
    // — log it loudly with a `context_serialization_failed: true` extension so
    // operators can spot it, but preserve the underlying error's HTTP status
    // by falling back to a context-less Problem rather than panicking the
    // request task.
    match modkit_canonical_errors::Problem::from_error(e) {
        Ok(canonical) => {
            let status =
                StatusCode::from_u16(canonical.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            Problem::new(status, canonical.title, canonical.detail)
                .with_type(canonical.problem_type)
                .with_context(canonical.context)
        }
        Err(err) => {
            // Built-in canonical contexts are expected to be Serialize-clean;
            // a failure here is a regression we want loud in the logs. We
            // intentionally do NOT panic via `debug_assert!` — the recovery
            // path is carefully designed to keep the request task alive in
            // prod, and panicking only under `cfg(debug_assertions)` would
            // turn every test exercising this branch into an opaque crash
            // instead of a diagnosable failure with the surrounding context.
            tracing::error!(
                error = %e,
                serialization_error = %err,
                "canonical Problem serialization failed; falling back to context-less Problem",
            );
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            Problem::new(status, e.title(), e.detail().to_owned()).with_context(serde_json::json!({
                "context_serialization_failed": true,
            }))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "handlers_tests.rs"]
mod handlers_tests;
