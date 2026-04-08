use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::api::operation_builder::OperationBuilder;

use super::AiChatLicense;
use crate::api::rest::handlers;

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
        .json_response(http::StatusCode::OK, "Turn found")
        .standard_errors(openapi)
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
    .summary("Retry a failed turn")
    .tag(API_TAG)
    .authenticated()
    .require_license_features([&AiChatLicense])
    .path_param("id", "Chat UUID")
    .path_param("request_id", "Turn request UUID")
    .handler(handlers::turns::retry_turn)
    .json_response(http::StatusCode::OK, "Turn retry initiated")
    .standard_errors(openapi)
    .register(router, openapi);

    // PATCH {prefix}/v1/chats/{id}/turns/{request_id}
    router = OperationBuilder::patch(format!("{prefix}/v1/chats/{{id}}/turns/{{request_id}}"))
        .operation_id("mini_chat.edit_turn")
        .summary("Edit a turn (user message)")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&AiChatLicense])
        .path_param("id", "Chat UUID")
        .path_param("request_id", "Turn request UUID")
        .handler(handlers::turns::edit_turn)
        .json_response(http::StatusCode::OK, "Turn edited")
        .standard_errors(openapi)
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
        .json_response(http::StatusCode::NO_CONTENT, "Turn deleted")
        .standard_errors(openapi)
        .register(router, openapi);

    router
}
