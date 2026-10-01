use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::{AiChatLicense, retry_after_header};
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "Mini Chat Reactions";

pub(super) fn register_reaction_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    prefix: &str,
) -> Router {
    // PUT {prefix}/v1/chats/{id}/messages/{msg_id}/reaction
    router = OperationBuilder::put(format!(
        "{prefix}/v1/chats/{{id}}/messages/{{msg_id}}/reaction"
    ))
    .operation_id("mini_chat.put_reaction")
    .summary("Set or update a reaction on a message")
    .tag(API_TAG)
    .authenticated()
    .require_license_features([&AiChatLicense])
    .path_param("id", "Chat UUID")
    .path_param("msg_id", "Message UUID")
    .json_request::<dto::SetReactionReq>(openapi, "Reaction data")
    .handler(handlers::reactions::put_reaction)
    .json_response_with_schema::<dto::ReactionDto>(openapi, http::StatusCode::OK, "Reaction set")
    .error_400(openapi)
    .error_401(openapi)
    .error_403(openapi)
    .error_404(openapi)
    .error_500(openapi)
    .error_503(openapi)
    .response_header(retry_after_header())
    .error_422(openapi)
    .register(router, openapi);

    // DELETE {prefix}/v1/chats/{id}/messages/{msg_id}/reaction
    router = OperationBuilder::delete(format!(
        "{prefix}/v1/chats/{{id}}/messages/{{msg_id}}/reaction"
    ))
    .operation_id("mini_chat.delete_reaction")
    .summary("Remove a reaction from a message")
    .tag(API_TAG)
    .authenticated()
    .require_license_features([&AiChatLicense])
    .path_param("id", "Chat UUID")
    .path_param("msg_id", "Message UUID")
    .handler(handlers::reactions::delete_reaction)
    .no_content_response(http::StatusCode::NO_CONTENT, "Reaction removed")
    .error_400(openapi)
    .error_401(openapi)
    .error_403(openapi)
    .error_404(openapi)
    .error_500(openapi)
    .error_503(openapi)
    .response_header(retry_after_header())
    .register(router, openapi);

    router
}
