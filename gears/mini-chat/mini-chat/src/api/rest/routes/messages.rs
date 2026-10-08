use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};

use super::{AiChatLicense, retry_after_header};
use crate::api::rest::{dto, handlers};
use crate::infra::db::odata_mapper::MessageField;

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
        .json_response_with_schema::<toolkit_odata::Page<dto::MessageDto>>(
            openapi,
            http::StatusCode::OK,
            "Paginated list of messages",
        )
        .with_odata_filter::<MessageField>()
        .with_odata_orderby::<MessageField>()
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .response_header(retry_after_header())
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
        .sse_json::<crate::api::rest::sse::MiniChatSseEvent>(
            openapi,
            "SSE stream of chat response events",
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

    router
}
