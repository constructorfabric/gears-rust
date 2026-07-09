//! `OperationBuilder` route registration for the foundation
//! `/usage-collector/v1/records` create + deactivation surface.
//! Every route is registered with `.no_license_required()` — the
//! foundation create surface is platform-internal substrate.

use axum::Router;
use toolkit::api::canonical_prelude::*;
use toolkit::api::operation_builder::OperationBuilderODataExt;
use toolkit::api::{OpenApiRegistry, OperationBuilder};
use usage_collector_sdk::UsageRecordFilterField;

use super::{dto, handlers};

const USAGE_RECORDS_TAG: &str = "Usage Records";

// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-api-post-records:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-ingestion:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-component-ingestion-gateway:p1
pub(super) fn register_usage_record_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // @cpt-begin:cpt-cf-usage-collector-dod-usage-emission-api-post-records:p1:inst-register-route-create-records
    router = OperationBuilder::post("/usage-collector/v1/records")
        .operation_id("usage_collector.create_usage_records")
        .summary("Create usage records")
        .description("Submit a batch of usage records for persistence.")
        .tag(USAGE_RECORDS_TAG)
        .authenticated()
        .no_license_required()
        .json_request::<dto::CreateUsageRecordsRequest>(openapi, "Usage-record create payload")
        .handler(handlers::handle_create_usage_records)
        .json_response_with_schema::<dto::CreateUsageRecordsResponse>(
            openapi,
            StatusCode::OK,
            "All records accepted",
        )
        .json_response_with_schema::<dto::CreateUsageRecordsResponse>(
            openapi,
            StatusCode::MULTI_STATUS,
            "At least one record was rejected; inspect each per-record outcome",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    // @cpt-end:cpt-cf-usage-collector-dod-usage-emission-api-post-records:p1:inst-register-route-create-records

    // @cpt-flow:cpt-cf-usage-collector-flow-usage-query-query-raw:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-query-fr-query-raw:p1
    router = OperationBuilder::get("/usage-collector/v1/records")
        .operation_id("usage_collector.list_usage_records")
        .summary("List usage records")
        .description("Keyset-paginated raw read over the persisted usage records.")
        .tag(USAGE_RECORDS_TAG)
        .query_param("gts_id", true, "Usage-type GTS instance id (mandatory)")
        .query_param(
            "metadata.<key>",
            false,
            "Repeated metadata-filter entries; OR within a key, AND across keys",
        )
        .query_param_typed(
            "limit",
            false,
            "Page size hint (rejected with 400 if above 1000)",
            "integer",
        )
        .query_param("cursor", false, "Opaque CursorV1 continuation token")
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-request-received
        .authenticated()
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-request-received
        .no_license_required()
        .handler(handlers::handle_list_usage_records)
        .json_response_with_schema::<toolkit_odata::Page<dto::UsageRecordDto>>(
            openapi,
            StatusCode::OK,
            "Usage records page",
        )
        .with_odata_filter::<UsageRecordFilterField>()
        .with_odata_orderby::<UsageRecordFilterField>()
        .with_odata_select()
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // @cpt-flow:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-query-fr-query-aggregation:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-query-api-post-records-aggregate:p1
    router = OperationBuilder::post("/usage-collector/v1/records/aggregate")
        .operation_id("usage_collector.query_aggregated_usage_records")
        .summary("Query server-side aggregated usage")
        .description("Server-side aggregation (`SUM` / `COUNT` / `MIN` / `MAX` / `AVG`) over the persisted usage records.")
        .tag(USAGE_RECORDS_TAG)
        .query_param("gts_id", true, "Usage-type GTS instance id (mandatory)")
        .query_param(
            "metadata.<key>",
            false,
            "Repeated metadata-filter entries; OR within a key, AND across keys",
        )
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-request-received
        .authenticated()
        // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-request-received
        .no_license_required()
        .json_request::<dto::QueryAggregatedUsageRecordsRequest>(
            openapi,
            "Aggregation operator + optional group-by dimensions",
        )
        .handler(handlers::handle_query_aggregated_usage_records)
        .json_response_with_schema::<dto::AggregationResultDto>(
            openapi,
            StatusCode::OK,
            "Aggregation result",
        )
        .with_odata_filter::<UsageRecordFilterField>()
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // @cpt-flow:cpt-cf-usage-collector-flow-usage-emission-get-record:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-api-get-records-id:p1
    // @cpt-begin:cpt-cf-usage-collector-dod-usage-emission-api-get-records-id:p1:inst-register-route-get-record
    router = OperationBuilder::get("/usage-collector/v1/records/{id}")
        .operation_id("usage_collector.get_usage_record")
        .summary("Get a usage record")
        .description("Read a single usage record by `id`.")
        .tag(USAGE_RECORDS_TAG)
        .path_param("id", "Usage-record UUID")
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-submit
        .authenticated()
        // @cpt-end:cpt-cf-usage-collector-flow-usage-emission-get-record:p1:inst-get-record-submit
        .no_license_required()
        .handler(handlers::handle_get_usage_record)
        .json_response_with_schema::<dto::UsageRecordDto>(
            openapi,
            StatusCode::OK,
            "The persisted usage record",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    // @cpt-end:cpt-cf-usage-collector-dod-usage-emission-api-get-records-id:p1:inst-register-route-get-record

    // @cpt-flow:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-event-deactivation-api-post-records-id-deactivate:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-event-deactivation-component-deactivation-handler:p1
    router = OperationBuilder::post("/usage-collector/v1/records/{id}/deactivate")
        .operation_id("usage_collector.deactivate_usage_record")
        .summary("Deactivate a usage record")
        .description("Deactivate a usage record by `id`.")
        .tag(USAGE_RECORDS_TAG)
        .path_param("id", "Usage-record UUID")
        // @cpt-begin:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-no-ctx
        // @cpt-begin:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-submit
        .authenticated()
        // @cpt-end:cpt-cf-usage-collector-flow-event-deactivation-deactivate-record:p1:inst-deactivate-record-submit
        // @cpt-end:cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization:p1:inst-algo-pdp-no-ctx
        .no_license_required()
        .handler(handlers::handle_deactivate_usage_record)
        .no_content_response(StatusCode::NO_CONTENT, "Deactivation succeeded")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "usage_records_tests.rs"]
mod usage_records_tests;
