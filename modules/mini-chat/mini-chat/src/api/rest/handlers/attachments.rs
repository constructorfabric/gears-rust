use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::Extension;
use axum::extract::Path;
use bytes::Bytes;
use futures::stream::Stream;
use modkit::api::prelude::*;
use modkit_security::SecurityContext;

use crate::api::rest::dto::AttachmentDetailDto;
use crate::domain::mime_validation::{
    MIME_OCTET_STREAM, infer_mime_from_extension, normalize_mime, remap_csv_to_plain, validate_mime,
};
use crate::module::AppServices;

// ── multer::Field → FileStream adapter ──────────────────────────────────

/// Wraps `multer::Field<'static>` as a [`FileStream`] with a kind-specific
/// size limit enforced during streaming.
///
/// `multer::Constraints` provides a coarse outer guard (max of file/image
/// limits). This wrapper enforces the fine-grained per-kind limit after
/// MIME validation determines whether the upload is a document or image.
/// Fused: stops after error or completion.
struct FieldStream {
    field: multer::Field<'static>,
    max_bytes: u64,
    total_bytes: u64,
    done: bool,
}

impl FieldStream {
    fn new(field: multer::Field<'static>, max_bytes: u64) -> Self {
        Self {
            field,
            max_bytes,
            total_bytes: 0,
            done: false,
        }
    }
}

impl Stream for FieldStream {
    type Item = Result<Bytes, Box<dyn std::error::Error + Send + Sync>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        match Pin::new(&mut this.field).poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                this.total_bytes += chunk.len() as u64;
                if this.total_bytes > this.max_bytes {
                    this.done = true;
                    tracing::info!(
                        total_bytes = this.total_bytes,
                        max_bytes = this.max_bytes,
                        "streaming upload: size limit exceeded"
                    );
                    Poll::Ready(Some(Err(Box::new(multer::Error::FieldSizeExceeded {
                        limit: this.max_bytes,
                        field_name: Some("file".to_owned()),
                    }))))
                } else {
                    Poll::Ready(Some(Ok(chunk)))
                }
            }
            Poll::Ready(Some(Err(e))) => {
                this.done = true;
                Poll::Ready(Some(Err(Box::new(e))))
            }
            Poll::Ready(None) => {
                this.done = true;
                tracing::debug!(
                    total_bytes = this.total_bytes,
                    "streaming upload: stream complete"
                );
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

// ── Handlers ────────────────────────────────────────────────────────────

/// POST /mini-chat/v1/chats/{id}/attachments
///
/// Streaming multipart upload with zero-copy size enforcement.
/// Uses `multer` directly (not Axum's `Multipart` extractor) so the field
/// is `'static` and can be wrapped as a `FileStream` without buffering.
/// `multer::Constraints` enforces the per-field size limit at the parser
/// level — oversize fields produce `multer::Error::FieldSizeExceeded`.
#[tracing::instrument(skip(svc, ctx, headers, body), fields(chat_id = %chat_id))]
pub(crate) async fn upload_attachment(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path(chat_id): Path<uuid::Uuid>,
    headers: http::HeaderMap,
    body: axum::body::Body,
) -> ApiResult<impl IntoResponse> {
    // 1. Resolve upload context (authz + model + limits) before reading body.
    let upload_ctx = svc.attachments.get_upload_context(&ctx, chat_id).await?;

    // 2. Parse multipart boundary from Content-Type header.
    let content_type = headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let boundary = multer::parse_boundary(content_type).map_err(|_| {
        Problem::new(
            http::StatusCode::BAD_REQUEST,
            "Missing Boundary",
            "Content-Type must be multipart/form-data with a boundary",
        )
    })?;

    // 3. Create multer::Multipart with per-field size constraint.
    //    We don't know the attachment kind yet (need to read headers first),
    //    so we use the larger of the two limits. The actual kind-specific
    //    limit is enforced after MIME validation if needed.
    let coarse_limit = upload_ctx
        .limits
        .max_file_bytes
        .max(upload_ctx.limits.max_image_bytes);
    let constraints = multer::Constraints::new().size_limit(
        multer::SizeLimit::new()
            .whole_stream(coarse_limit + 64 * 1024) // file + multipart overhead
            .for_field("file", coarse_limit),
    );
    let mut multipart =
        multer::Multipart::with_constraints(body.into_data_stream(), boundary, constraints);

    // 4. Find the "file" field.
    let field = loop {
        match multipart.next_field().await.map_err(|e| {
            Problem::new(
                http::StatusCode::BAD_REQUEST,
                "Multipart Error",
                format!("Failed to read multipart field: {e}"),
            )
        })? {
            Some(f) if f.name() == Some("file") => break f,
            Some(_) => {}
            None => {
                return Err(Problem::new(
                    http::StatusCode::BAD_REQUEST,
                    "Missing File",
                    "No file field found in multipart request",
                ));
            }
        }
    };

    // 5. Extract headers (available before body bytes).
    let filename = field
        .file_name()
        .map_or_else(|| "upload".to_owned(), ToString::to_string);
    // Truncate to 255 chars (DB column is VARCHAR(255)).
    let filename = if filename.len() > 255 {
        filename[..255].to_owned()
    } else {
        filename
    };
    let raw_ct = field
        .content_type()
        .map(ToString::to_string)
        .ok_or_else(|| {
            Problem::new(
                http::StatusCode::BAD_REQUEST,
                "Missing Content-Type",
                "File field has no content type",
            )
        })?;

    // 6. MIME validation (from field headers, before reading body bytes).
    let effective_ct = if normalize_mime(&raw_ct) == MIME_OCTET_STREAM {
        infer_mime_from_extension(&filename).unwrap_or(&raw_ct)
    } else {
        &raw_ct
    };
    let effective_ct = if upload_ctx.allow_csv_upload {
        remap_csv_to_plain(effective_ct).unwrap_or(effective_ct)
    } else {
        effective_ct
    };
    let validated = validate_mime(effective_ct)?;
    let is_document = validated.kind == crate::domain::mime_validation::AttachmentKind::Document;

    // 7. Kind-specific size limit (multer's coarse constraint uses the max).
    let max_bytes = if is_document {
        upload_ctx.limits.max_file_bytes
    } else {
        upload_ctx.limits.max_image_bytes
    };

    // 8. Wrap field as FileStream with kind-specific size enforcement.
    //    Zero buffering in handler — multer provides the coarse guard,
    //    FieldStream enforces the fine-grained per-kind limit.
    let file_stream: crate::domain::ports::FileStream =
        Box::pin(FieldStream::new(field, max_bytes));

    // 8b. Best-effort size_hint from Content-Length (enables aggregate check).
    let size_hint = headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    // 8c. Pre-flight: reject obvious oversize uploads from Content-Length.
    //     Content-Length includes multipart framing, so subtract the same
    //     overhead budget used by the multer constraints (step 3).
    //     FieldStream remains the authoritative check for borderline cases,
    //     but this avoids a DB round-trip for clearly oversize uploads.
    const MULTIPART_OVERHEAD: u64 = 64 * 1024;
    if let Some(cl) = size_hint {
        let estimated_file_bytes = cl.saturating_sub(MULTIPART_OVERHEAD);
        if estimated_file_bytes > max_bytes {
            let kind = if is_document { "document" } else { "image" };
            return Err(Problem::new(
                http::StatusCode::PAYLOAD_TOO_LARGE,
                "file_too_large",
                format!(
                    "Uploaded {kind} (~{estimated_file_bytes} bytes) exceeds the {kind} size limit of {max_bytes} bytes"
                ),
            )
            .with_code("file_too_large".to_owned()));
        }
    }

    // 9. Call domain service with pre-resolved context.
    let row = svc
        .attachments
        .upload_file(
            &ctx,
            chat_id,
            upload_ctx,
            filename,
            validated.mime,
            validated.kind,
            file_stream,
            size_hint,
        )
        .await?;

    Ok((
        http::StatusCode::CREATED,
        Json(AttachmentDetailDto::from(row)),
    )
        .into_response())
}

/// GET /mini-chat/v1/chats/{id}/attachments/{attachment_id}
#[tracing::instrument(skip(svc, ctx), fields(chat_id = %chat_id, attachment_id = %attachment_id))]
pub(crate) async fn get_attachment(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path((chat_id, attachment_id)): Path<(uuid::Uuid, uuid::Uuid)>,
) -> ApiResult<JsonBody<AttachmentDetailDto>> {
    let row = svc
        .attachments
        .get_attachment(&ctx, chat_id, attachment_id)
        .await?;
    Ok(Json(AttachmentDetailDto::from(row)))
}

/// DELETE /mini-chat/v1/chats/{id}/attachments/{attachment_id}
#[tracing::instrument(skip(svc, ctx), fields(chat_id = %chat_id, attachment_id = %attachment_id))]
pub(crate) async fn delete_attachment(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path((chat_id, attachment_id)): Path<(uuid::Uuid, uuid::Uuid)>,
) -> ApiResult<StatusCode> {
    svc.attachments
        .delete_attachment(&ctx, chat_id, attachment_id)
        .await?;
    Ok(http::StatusCode::NO_CONTENT)
}
