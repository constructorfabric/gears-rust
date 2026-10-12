use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::{AiChatLicense, retry_after_header};
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "Mini Chat Turns";

pub(super) fn register_turn_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    prefix: &str,
) -> Router {
    // GET {prefix}/v1/chats/{id}/turns/{request_id}
    router = OperationBuilder::get(format!("{prefix}/v1/chats/{{id}}/turns/{{request_id}}"))
        .operation_id("mini_chat.get_turn")
        .summary("Get a turn by request ID")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&AiChatLicense])
        .path_param("id", "Chat UUID")
        .path_param("request_id", "Turn request UUID")
        .handler(handlers::turns::get_turn)
        .json_response_with_schema::<dto::TurnStatusResponse>(
            openapi,
            http::StatusCode::OK,
            "Turn found",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .response_header(retry_after_header())
        .register(router, openapi);

    // POST {prefix}/v1/chats/{id}/turns/{request_id}/retry
    //
    // TODO: DESIGN.md specifies Google-style `{request_id}:retry` (AIP-136), but
    // axum 0.8 pins matchit =0.8.4 which cannot split `{param}:suffix` in one
    // segment. Using `/retry` as a sub-resource until axum bumps matchit ≥0.8.6
    // which adds suffix support.
    // Tracking: https://github.com/tokio-rs/axum/issues/3140
    router = OperationBuilder::post(format!(
        "{prefix}/v1/chats/{{id}}/turns/{{request_id}}/retry"
    ))
    .operation_id("mini_chat.retry_turn")
    .summary("Retry the latest terminal turn and stream the new response via SSE")
    .tag(API_TAG)
    .authenticated()
    .require_license_features([&AiChatLicense])
    .path_param("id", "Chat UUID")
    .path_param("request_id", "Turn request UUID")
    .handler(handlers::turns::retry_turn)
    .sse_json::<crate::api::rest::sse::MiniChatSseEvent>(
        openapi,
        "SSE stream of the regenerated response",
    )
    .error_400(openapi)
    .error_401(openapi)
    .error_403(openapi)
    .error_404(openapi)
    .error_409(openapi)
    .error_429(openapi)
    .error_500(openapi)
    .error_503(openapi)
    .response_header(retry_after_header())
    .register(router, openapi);

    // PATCH {prefix}/v1/chats/{id}/turns/{request_id}
    router = OperationBuilder::patch(format!("{prefix}/v1/chats/{{id}}/turns/{{request_id}}"))
        .operation_id("mini_chat.edit_turn")
        .summary("Edit the latest turn's user message and stream the new response via SSE")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&AiChatLicense])
        .path_param("id", "Chat UUID")
        .path_param("request_id", "Turn request UUID")
        .json_request::<dto::EditTurnRequest>(openapi, "Replacement user message")
        .handler(handlers::turns::edit_turn)
        .sse_json::<crate::api::rest::sse::MiniChatSseEvent>(
            openapi,
            "SSE stream of the regenerated response",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_429(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .response_header(retry_after_header())
        .error_422(openapi)
        .register(router, openapi);

    // DELETE {prefix}/v1/chats/{id}/turns/{request_id}
    router = OperationBuilder::delete(format!("{prefix}/v1/chats/{{id}}/turns/{{request_id}}"))
        .operation_id("mini_chat.delete_turn")
        .summary("Delete a turn")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&AiChatLicense])
        .path_param("id", "Chat UUID")
        .path_param("request_id", "Turn request UUID")
        .handler(handlers::turns::delete_turn)
        .no_content_response(http::StatusCode::NO_CONTENT, "Turn deleted")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .response_header(retry_after_header())
        .register(router, openapi);

    router
}
