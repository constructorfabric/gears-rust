//! `OperationBuilder` route registration.

use std::sync::Arc;

use axum::Router;
use toolkit::api::canonical_prelude::*;
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use super::{dto, handlers};
use crate::domain::Service;

const RESPONSES_TAG: &str = "LLM Gateway";

/// Register the REST routes and attach the shared
/// `Extension<Arc<Service>>` layer.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<Service>,
) -> Router {
    OperationBuilder::post("/llm-gateway/v1/responses")
        .operation_id("llm_gateway.responses.create")
        .summary("Create a response")
        .description(
            "Open Responses create-response. `stream=false` returns the assembled response as JSON; \
             `stream=true` returns a `text/event-stream` of streaming events closed by `data: [DONE]`.",
        )
        .tag(RESPONSES_TAG)
        .authenticated()
        .no_license_required()
        .json_request::<dto::CreateResponseRequest>(openapi, "Open Responses create-response body")
        .handler(handlers::handle_create_response)
        .json_response_with_schema::<dto::ResponseResourceDto>(
            openapi,
            StatusCode::OK,
            "The created response (`stream=false`)",
        )
        .sse_json::<dto::StreamingEventDto>(
            openapi,
            "SSE stream of Open Responses streaming events (`stream=true`), closed by `[DONE]`",
        )
        .standard_errors(openapi)
        .problem_response(
            openapi,
            StatusCode::NOT_IMPLEMENTED,
            "Provider not supported",
        )
        .error_503(openapi)
        .register(router, openapi)
        .layer(axum::Extension(service))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "routes_tests.rs"]
mod routes_tests;
