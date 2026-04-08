use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::api::operation_builder::OperationBuilder;

use super::AiChatLicense;
use crate::api::rest::handlers;
use crate::api::rest::handlers::quota::QuotaStatusResponse;

const API_TAG: &str = "Mini Chat Quotas";

pub(super) fn register_quota_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    prefix: &str,
) -> Router {
    // GET {prefix}/v1/quota/status
    router = OperationBuilder::get(format!("{prefix}/v1/quota/status"))
        .operation_id("mini_chat.get_quota_status")
        .summary("Get quota status for the authenticated user")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&AiChatLicense])
        .handler(handlers::quota::get_quota_status)
        .json_response_with_schema::<QuotaStatusResponse>(
            openapi,
            http::StatusCode::OK,
            "Quota status with remaining percentages and warning flags",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router
}
