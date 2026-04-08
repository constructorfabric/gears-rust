use axum::Router;
use axum::extract::DefaultBodyLimit;
use modkit::api::OpenApiRegistry;
use modkit::api::operation_builder::OperationBuilder;

use super::AiChatLicense;
use crate::api::rest::handlers;

const API_TAG: &str = "Mini Chat Attachments";

/// Coarse outer body-size guard for the upload route.
///
/// Set to the largest allowed upload (25 MiB for files) plus 64 KiB for
/// multipart overhead. The fine-grained per-kind limit is enforced by the
/// handler's streaming byte counter.
///
/// This overrides the API gateway's global `DefaultBodyLimit` (16 MiB)
/// so that file uploads up to 25 MiB are not rejected by the framework.
const UPLOAD_BODY_LIMIT: usize = 25 * 1024 * 1024 + 65536;

pub(super) fn register_attachment_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    prefix: &str,
) -> Router {
    // POST {prefix}/v1/chats/{id}/attachments (multipart/form-data)
    // DefaultBodyLimit overrides the gateway's global 16 MiB limit for this route.
    router = OperationBuilder::post(format!("{prefix}/v1/chats/{{id}}/attachments"))
        .operation_id("mini_chat.upload_attachment")
        .summary("Upload an attachment to a chat")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&AiChatLicense])
        .path_param("id", "Chat UUID")
        .handler(handlers::attachments::upload_attachment)
        .json_response(http::StatusCode::CREATED, "Attachment uploaded")
        .error_415(openapi)
        .standard_errors(openapi)
        .register(router, openapi)
        .layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT));

    // GET {prefix}/v1/chats/{id}/attachments/{attachment_id}
    router = OperationBuilder::get(format!(
        "{prefix}/v1/chats/{{id}}/attachments/{{attachment_id}}"
    ))
    .operation_id("mini_chat.get_attachment")
    .summary("Get attachment metadata")
    .tag(API_TAG)
    .authenticated()
    .require_license_features([&AiChatLicense])
    .path_param("id", "Chat UUID")
    .path_param("attachment_id", "Attachment UUID")
    .handler(handlers::attachments::get_attachment)
    .json_response(http::StatusCode::OK, "Attachment metadata")
    .standard_errors(openapi)
    .register(router, openapi);

    // DELETE {prefix}/v1/chats/{id}/attachments/{attachment_id}
    router = OperationBuilder::delete(format!(
        "{prefix}/v1/chats/{{id}}/attachments/{{attachment_id}}"
    ))
    .operation_id("mini_chat.delete_attachment")
    .summary("Delete an attachment")
    .tag(API_TAG)
    .authenticated()
    .require_license_features([&AiChatLicense])
    .path_param("id", "Chat UUID")
    .path_param("attachment_id", "Attachment UUID")
    .handler(handlers::attachments::delete_attachment)
    .json_response(http::StatusCode::NO_CONTENT, "Attachment deleted")
    .error_409(openapi)
    .standard_errors(openapi)
    .register(router, openapi);

    router
}
