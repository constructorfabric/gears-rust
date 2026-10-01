use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::{AiChatLicense, retry_after_header};
use crate::api::rest::{dto, handlers};

const API_TAG: &str = "Mini Chat Attachments";

pub(super) fn register_attachment_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    prefix: &str,
) -> Router {
    // POST {prefix}/v1/chats/{id}/attachments (multipart/form-data)
    // The request size cap is the api-gateway's `body_limit_bytes`; the
    // per-kind file limit is the handler's streaming byte counter.
    router = OperationBuilder::post(format!("{prefix}/v1/chats/{{id}}/attachments"))
        .operation_id("mini_chat.upload_attachment")
        .multipart_file_request("file", Some("File to upload"))
        .summary("Upload an attachment to a chat")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&AiChatLicense])
        .path_param("id", "Chat UUID")
        .handler(handlers::attachments::upload_attachment)
        .json_response_with_schema::<dto::AttachmentDetailDto>(
            openapi,
            http::StatusCode::CREATED,
            "Attachment uploaded and processed",
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
    .json_response_with_schema::<dto::AttachmentDetailDto>(
        openapi,
        http::StatusCode::OK,
        "Attachment metadata",
    )
    .error_400(openapi)
    .error_401(openapi)
    .error_403(openapi)
    .error_404(openapi)
    .error_500(openapi)
    .error_503(openapi)
    .response_header(retry_after_header())
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
    .no_content_response(http::StatusCode::NO_CONTENT, "Attachment deleted")
    .error_409(openapi)
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
