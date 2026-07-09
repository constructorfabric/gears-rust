//! `OperationBuilder` route registration for the foundation
//! `/usage-collector/v1/usage-types` catalog. Every route is registered
//! with `.no_license_required()` — the foundation catalog is
//! platform-internal substrate.

use axum::Router;
use toolkit::api::canonical_prelude::*;
use toolkit::api::operation_builder::OperationBuilderODataExt;
use toolkit::api::{OpenApiRegistry, OperationBuilder};
use usage_collector_sdk::UsageTypeFilterField;

use super::{dto, handlers};

const USAGE_TYPE_CATALOG_TAG: &str = "Usage Types";

pub(super) fn register_usage_type_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    router = OperationBuilder::post("/usage-collector/v1/usage-types")
        .operation_id("usage_collector.create_usage_type")
        .summary("Create a usage type")
        .description("Create a new entry in the usage-type catalog.")
        .tag(USAGE_TYPE_CATALOG_TAG)
        .authenticated()
        .no_license_required()
        .json_request::<dto::CreateUsageTypeRequest>(openapi, "Usage-type creation payload")
        .handler(handlers::handle_create_usage_type)
        .json_response_with_schema::<dto::UsageTypeDto>(
            openapi,
            StatusCode::CREATED,
            "Usage type created",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/usage-collector/v1/usage-types")
        .operation_id("usage_collector.list_usage_types")
        .summary("List usage types")
        .description("List registered usage types with OData filtering.")
        .tag(USAGE_TYPE_CATALOG_TAG)
        .query_param_typed("limit", false, "Page size hint", "integer")
        .query_param("cursor", false, "Opaque CursorV1 continuation token")
        .authenticated()
        .no_license_required()
        .handler(handlers::handle_list_usage_types)
        .json_response_with_schema::<toolkit_odata::Page<dto::UsageTypeDto>>(
            openapi,
            StatusCode::OK,
            "Usage types page",
        )
        .with_odata_filter::<UsageTypeFilterField>()
        .with_odata_orderby::<UsageTypeFilterField>()
        .with_odata_select()
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/usage-collector/v1/usage-types/{gts_id}")
        .operation_id("usage_collector.get_usage_type")
        .summary("Get a usage type")
        .description("Get a single usage type by `gts_id`.")
        .tag(USAGE_TYPE_CATALOG_TAG)
        .path_param("gts_id", "Usage-type GTS instance id")
        .authenticated()
        .no_license_required()
        .handler(handlers::handle_get_usage_type)
        .json_response_with_schema::<dto::UsageTypeDto>(
            openapi,
            StatusCode::OK,
            "Usage-type record",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/usage-collector/v1/usage-types/{gts_id}")
        .operation_id("usage_collector.delete_usage_type")
        .summary("Delete a usage type")
        .description("Delete a usage type by `gts_id`.")
        .tag(USAGE_TYPE_CATALOG_TAG)
        .path_param("gts_id", "Usage-type GTS instance id")
        .authenticated()
        .no_license_required()
        .handler(handlers::handle_delete_usage_type)
        .no_content_response(StatusCode::NO_CONTENT, "Usage type deleted")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "usage_types_tests.rs"]
mod usage_types_tests;
