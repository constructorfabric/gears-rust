use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::api::operation_builder::OperationBuilder;

use super::AiChatLicense;
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "Mini Chat Messages";

pub(super) fn register_message_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    prefix: &str,
) -> Router {
    // GET {prefix}/v1/chats/{id}/messages
    router = OperationBuilder::get(format!("{prefix}/v1/chats/{{id}}/messages"))
        .operation_id("mini_chat.list_messages")
        .summary("List messages in a chat")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&AiChatLicense])
        .path_param("id", "Chat UUID")
        .query_param_typed(
            "limit",
            false,
            "Maximum number of messages to return",
            "integer",
        )
        .query_param("cursor", false, "Cursor for pagination")
        .handler(handlers::messages::list_messages)
        .json_response_with_schema::<modkit_odata::Page<dto::MessageDto>>(
            openapi,
            http::StatusCode::OK,
            "Paginated list of messages",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // POST {prefix}/v1/chats/{id}/messages:stream
    router = OperationBuilder::post(format!("{prefix}/v1/chats/{{id}}/messages:stream"))
        .operation_id("mini_chat.stream_message")
        .summary("Send a message and stream the response via SSE")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&AiChatLicense])
        .path_param("id", "Chat UUID")
        .json_request::<dto::StreamMessageRequest>(openapi, "Message to send")
        .handler(handlers::messages::stream_message)
        .sse_json::<crate::domain::stream_events::StreamEvent>(
            openapi,
            "SSE stream of chat response events",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router
}
