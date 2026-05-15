//! Route registration for the usage-collector REST API.

use std::sync::Arc;

use axum::{Extension, Router};

use modkit::api::operation_builder::LicenseFeature;
use modkit::api::{OpenApiRegistry, OperationBuilder};
use usage_emitter::UsageEmitterRuntimeV1;

use crate::domain::Service;

use super::dto::{CreateUsageRecordRequest, ModuleConfigResponse};
use super::handlers;

const API_TAG: &str = "Usage Collector";

struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        "gts.cf.core.lic.feat.v1~cf.core.global.base.v1"
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
        .require_license_features::<License>([])
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
        .require_license_features::<License>([])
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

    router.layer(Extension(runtime)).layer(Extension(service))
}
