//! Route registration for the usage-collector REST API.

use std::sync::Arc;

use axum::{Extension, Router};

use modkit::api::operation_builder::LicenseFeature;
use modkit::api::{OpenApiRegistry, OperationBuilder};
use usage_emitter::UsageEmitterRuntimeV1;

use crate::domain::Service;

use super::dto::{
    AggregationResultDto, CreateUsageRecordRequest, ModuleConfigResponse, RawQueryResponse,
};
use super::handlers;

const API_TAG: &str = "Usage Collector";

/// License feature literal that gates the usage-collector REST surface.
///
/// Exposed at `pub(crate)` so unit tests can assert wiring without forcing
/// the [`License`] marker type itself to leak outside the module — tests pin
/// the literal against this constant, route registration funnels through
/// [`License::as_ref`] which also returns it (FEATURE 0003 `DoD` §5).
pub(crate) const LICENSE_FEATURE: &str = "gts.cf.core.lic.feat.v1~cf.core.global.base.v1";

/// Marker type carrying [`LICENSE_FEATURE`] for
/// `OperationBuilder::require_license_features`. The framework's license
/// gate denies the request with `403 Forbidden` BEFORE any PDP call when the
/// feature is missing from the caller's tenant license.
struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// Register all REST routes for the usage-collector module.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    runtime: Arc<dyn UsageEmitterRuntimeV1>,
    service: Arc<Service>,
) -> Router {
    let router = OperationBuilder::post("/usage-collector/v1/records")
        .operation_id("usage_collector.create_usage_record")
        .summary("Create a usage record")
        .description(
            "Accepts a usage record payload and delegates storage to the configured storage plugin.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&License])
        .json_request::<CreateUsageRecordRequest>(openapi, "Usage record to create")
        .allow_content_types(&["application/json"])
        .handler(handlers::handle_create_usage_record)
        .no_content_response(http::StatusCode::NO_CONTENT, "Record accepted and stored")
        // Validation errors (metadata > 8 KiB, missing/empty idempotency_key for counter,
        // non-finite value, negative counter value, metric-kind mismatch).
        .error_400(openapi)
        // Stale authorization handle (TTL expired between authorize and enqueue).
        .error_401(openapi)
        // PDP denial / tenant/resource/module/subject mismatch.
        .error_403(openapi)
        // Malformed payload (non-finite numeric literals, invalid nested subject shape, etc.).
        .error_422(openapi)
        // Rate limiting propagated from the storage plugin.
        .error_429(openapi)
        // Internal failures from the emitter or storage plugin.
        .error_500(openapi)
        // Outbox enqueue failure or transient transport error.
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
        )
        // Plugin call timed out.
        .problem_response(openapi, http::StatusCode::GATEWAY_TIMEOUT, "Gateway Timeout")
        .register(router, openapi);

    let router = OperationBuilder::get("/usage-collector/v1/modules/{module_name}/config")
        .operation_id("usage_collector.get_module_config")
        .summary("Get module configuration")
        .description(
            "Returns the allowed metrics (and future configuration) for the specified module.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&License])
        .path_param("module_name", "Module name")
        .handler(handlers::handle_get_module_config)
        .json_response_with_schema::<ModuleConfigResponse>(
            openapi,
            http::StatusCode::OK,
            "Module configuration",
        )
        // Missing or invalid bearer token (authenticated route).
        .error_401(openapi)
        // Unknown module name.
        .error_404(openapi)
        // Internal failures resolving plugin / fetching config.
        .error_500(openapi)
        .register(router, openapi);

    // GET /usage-collector/v1/aggregated — aggregated usage query.
    //
    // The handler emits `inst-agg-8c` as a 503 envelope via the canonical
    // Problem mapper on every non-`PermissionDenied` plugin error, so the
    // OpenAPI registry declares 503 alongside 400/403/500 — generated clients
    // need to handle it.
    let router = OperationBuilder::get("/usage-collector/v1/aggregated")
        .operation_id("usage_collector.query_aggregated")
        .summary("Aggregated usage query")
        .description(
            "Aggregates usage records over an arbitrary time range with optional grouping and filtering. \
             Tenant scope is derived from SecurityContext; the request MUST NOT supply a tenant_id query parameter.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&License])
        .handler(handlers::handle_query_aggregated)
        .json_response_with_schema::<Vec<AggregationResultDto>>(
            openapi,
            http::StatusCode::OK,
            "Aggregation results",
        )
        // Validation failure (missing/malformed `from`/`to`, time range too wide, time-bucket
        // without `bucket_size`, string filter exceeding MAX_FILTER_STRING_LEN, ...).
        .error_400(openapi)
        // PDP denied or returned non-Denied (fail-closed); license gate denial.
        .error_403(openapi)
        // Internal handler / mapping failure.
        .error_500(openapi)
        // Storage plugin error (any non-`PermissionDenied` `DomainError`) — see `inst-agg-8c`.
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
        )
        .register(router, openapi);

    // GET /usage-collector/v1/raw — raw paginated usage query.
    //
    // Same OpenAPI registration policy as `/aggregated`: 400/403/500/503 declared.
    let router = OperationBuilder::get("/usage-collector/v1/raw")
        .operation_id("usage_collector.query_raw")
        .summary("Raw paginated usage query")
        .description(
            "Returns raw usage records over an arbitrary time range with optional filters and keyset pagination. \
             Tenant scope is derived from SecurityContext; the request MUST NOT supply a tenant_id query parameter.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&License])
        .handler(handlers::handle_query_raw)
        .json_response_with_schema::<RawQueryResponse>(
            openapi,
            http::StatusCode::OK,
            "Page of usage records",
        )
        // Validation failure (missing/malformed `from`/`to`, cursor decode failure, cursor
        // outside `[from, to]`, page_size out of range, string filter too long, ...).
        .error_400(openapi)
        // PDP denied or returned non-Denied (fail-closed); license gate denial.
        .error_403(openapi)
        // Internal handler / mapping failure.
        .error_500(openapi)
        // Storage plugin error (any non-`PermissionDenied` `DomainError`) — see `inst-raw-8b`.
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
        )
        .register(router, openapi);

    router.layer(Extension(runtime)).layer(Extension(service))
}
