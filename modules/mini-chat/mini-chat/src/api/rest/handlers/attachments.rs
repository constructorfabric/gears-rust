use std::sync::Arc;

use axum::Extension;
use axum::extract::{Multipart, Path};
use axum::response::IntoResponse;
use bytes::BytesMut;
use modkit::api::prelude::*;
use modkit_security::SecurityContext;

use crate::api::rest::dto::{AttachmentResponseDto, UploadAttachmentResponseDto};
use crate::api::rest::error::{domain_error_to_problem, domain_error_to_response};
use crate::module::AppServices;

/// POST /mini-chat/v1/chats/{id}/attachments
///
/// Returns `axum::response::Response` directly (instead of `ApiResult`) because the
/// `UploadQuotaExceeded` error must serialize as `QuotaExceededError` (`OpenAPI` schema)
/// rather than as `Problem`.
pub(crate) async fn upload_attachment(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path(chat_id): Path<uuid::Uuid>,
    mut multipart: Multipart,
) -> Result<(StatusCode, axum::Json<UploadAttachmentResponseDto>), axum::response::Response> {
    let max_bytes = svc.attachments.max_upload_size_bytes();

    let mut filename: Option<String> = None;
    let mut content_type: Option<String> = None;
    let mut file_bytes: Option<bytes::Bytes> = None;

    while let Some(mut field) = multipart.next_field().await.map_err(|e| {
        Problem::new(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            format!("Invalid multipart: {e}"),
        )
        .into_response()
    })? {
        let field_name = field.name().unwrap_or("").to_owned();
        if field_name == "file" {
            filename = field.file_name().map(ToString::to_string);
            content_type = field.content_type().map(ToString::to_string);

            // Stream chunks with early abort to avoid reading oversized files fully into memory.
            let mut buf = BytesMut::new();
            while let Some(chunk) = field.chunk().await.map_err(|e| {
                Problem::new(
                    StatusCode::BAD_REQUEST,
                    "Bad Request",
                    format!("Failed to read file: {e}"),
                )
                .into_response()
            })? {
                if buf.len() + chunk.len() > max_bytes {
                    return Err(Problem::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "File Too Large",
                        format!("File exceeds maximum size of {max_bytes} bytes"),
                    )
                    .into_response());
                }
                buf.extend_from_slice(&chunk);
            }
            file_bytes = Some(buf.freeze());
            break;
        }
    }

    let filename = filename.ok_or_else(|| {
        Problem::new(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "Missing filename in file field",
        )
        .into_response()
    })?;

    crate::infra::oagw::files_client::validate_filename(&filename)
        .map_err(|e| domain_error_to_response(&e))?;

    let content_type = content_type.ok_or_else(|| {
        Problem::new(
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "No content type for file",
        )
        .into_response()
    })?;
    let file_bytes = file_bytes.ok_or_else(|| {
        Problem::new(StatusCode::BAD_REQUEST, "Bad Request", "No file data found").into_response()
    })?;
    if file_bytes.is_empty() {
        return Err(
            Problem::new(StatusCode::BAD_REQUEST, "Bad Request", "Empty file upload")
                .into_response(),
        );
    }

    #[allow(clippy::cast_possible_wrap)] // body limit caps file size well under i64::MAX
    let size_bytes = file_bytes.len() as i64;

    let attachment = svc
        .attachments
        .upload_attachment(
            &ctx,
            chat_id,
            filename,
            content_type,
            size_bytes,
            file_bytes,
        )
        .await
        .map_err(|e| domain_error_to_response(&e))?;

    Ok((
        StatusCode::CREATED,
        axum::Json(UploadAttachmentResponseDto::pending(attachment.id)),
    ))
}

/// GET /mini-chat/v1/chats/{id}/attachments/{attachment_id}
pub(crate) async fn get_attachment(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path((chat_id, attachment_id)): Path<(uuid::Uuid, uuid::Uuid)>,
) -> ApiResult<(StatusCode, axum::Json<AttachmentResponseDto>)> {
    let attachment = svc
        .attachments
        .get_attachment(&ctx, chat_id, attachment_id)
        .await
        .map_err(|e| domain_error_to_problem(&e))?;

    Ok((
        StatusCode::OK,
        axum::Json(AttachmentResponseDto::from(attachment)),
    ))
}
