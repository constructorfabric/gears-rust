use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::api::operation_builder::OperationBuilder;

use super::AiChatLicense;
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
    .standard_errors(openapi)
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
    .json_response(http::StatusCode::NO_CONTENT, "Reaction removed")
    .standard_errors(openapi)
    .register(router, openapi);

    router
}
