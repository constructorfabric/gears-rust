//! HTTP DTOs (serde/utoipa) — REST-only request and response types.
//!
//! All REST DTOs live here; SDK `models.rs` stays transport-agnostic.
//! Provide `From` conversions between SDK models and DTOs in this file.
//!
//! Stream event types live in `domain::stream_events`; SSE wire conversion
//! and ordering enforcement live in `api::rest::sse`.

use crate::domain::models::{AttachmentSummary, ChatDetail, ImgThumbnail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use mini_chat_sdk::models::{Attachment, AttachmentKind, AttachmentStatus};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

// ════════════════════════════════════════════════════════════════════════════
// Chat CRUD DTOs
// ════════════════════════════════════════════════════════════════════════════

/// Request DTO for creating a new chat.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct CreateChatReq {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Request DTO for updating a chat title.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct UpdateChatReq {
    pub title: String,
}

/// Response DTO for chat details.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct ChatDetailDto {
    pub id: Uuid,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub is_temporary: bool,
    pub message_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<ChatDetail> for ChatDetailDto {
    fn from(d: ChatDetail) -> Self {
        Self {
            id: d.id,
            model: d.model,
            title: d.title,
            is_temporary: d.is_temporary,
            message_count: d.message_count,
            created_at: d.created_at,
            updated_at: d.updated_at,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Message DTOs
// ════════════════════════════════════════════════════════════════════════════

/// Response DTO for a message in the list endpoint.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct MessageDto {
    pub id: Uuid,
    pub request_id: Uuid,
    pub role: String,
    pub content: String,
    pub attachments: Vec<AttachmentSummaryDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<crate::domain::models::Message> for MessageDto {
    fn from(m: crate::domain::models::Message) -> Self {
        Self {
            id: m.id,
            request_id: m.request_id,
            role: m.role,
            content: m.content,
            attachments: m
                .attachments
                .into_iter()
                .map(AttachmentSummaryDto::from)
                .collect(),
            model: m.model,
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            created_at: m.created_at,
        }
    }
}

/// Lightweight attachment metadata embedded in Message responses.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct AttachmentSummaryDto {
    pub attachment_id: Uuid,
    pub kind: String,
    pub filename: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub img_thumbnail: Option<ImgThumbnailDto>,
}

impl From<AttachmentSummary> for AttachmentSummaryDto {
    fn from(a: AttachmentSummary) -> Self {
        Self {
            attachment_id: a.attachment_id,
            kind: a.kind,
            filename: a.filename,
            status: a.status,
            img_thumbnail: a.img_thumbnail.map(ImgThumbnailDto::from),
        }
    }
}

/// Server-generated preview thumbnail for an image attachment.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct ImgThumbnailDto {
    pub content_type: String,
    pub width: i32,
    pub height: i32,
    pub data_base64: String,
}

impl From<ImgThumbnail> for ImgThumbnailDto {
    fn from(t: ImgThumbnail) -> Self {
        Self {
            content_type: t.content_type,
            width: t.width,
            height: t.height,
            data_base64: t.data_base64,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Reaction DTOs
// ════════════════════════════════════════════════════════════════════════════

/// Request DTO for setting a reaction.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(request)]
pub struct SetReactionReq {
    pub reaction: String,
}

/// Response DTO for a reaction.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct ReactionDto {
    pub message_id: Uuid,
    pub reaction: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<crate::domain::models::Reaction> for ReactionDto {
    fn from(r: crate::domain::models::Reaction) -> Self {
        Self {
            message_id: r.message_id,
            reaction: r.kind.as_str().to_owned(),
            created_at: r.created_at,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Model DTOs
// ════════════════════════════════════════════════════════════════════════════

/// Response DTO for a single model.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct ModelDto {
    pub model_id: String,
    pub display_name: String,
    pub tier: String,
    pub multiplier_display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub multimodal_capabilities: Vec<String>,
    pub context_window: u32,
}

impl From<crate::domain::models::ResolvedModel> for ModelDto {
    fn from(m: crate::domain::models::ResolvedModel) -> Self {
        Self {
            model_id: m.model_id,
            display_name: m.display_name,
            tier: m.tier,
            multiplier_display: m.multiplier_display,
            description: m.description,
            multimodal_capabilities: m.multimodal_capabilities,
            context_window: m.context_window,
        }
    }
}

/// Response DTO for the model list endpoint.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct ModelListDto {
    pub items: Vec<ModelDto>,
}

// ════════════════════════════════════════════════════════════════════════════
// Streaming request DTOs
// ════════════════════════════════════════════════════════════════════════════

/// Request body for `POST /v1/chats/{id}/messages:stream`.
#[derive(Debug, Clone, serde::Deserialize, ToSchema)]
pub struct StreamMessageRequest {
    /// Message content (must be non-empty).
    pub content: String,
    /// Client-generated idempotency key (UUID v4). Optional in P1.
    #[serde(default)]
    pub request_id: Option<uuid::Uuid>,
    /// Attachment IDs to include (displayed in UI, images go to multimodal input).
    #[serde(default)]
    pub attachment_ids: Vec<uuid::Uuid>,
    /// Document IDs for retrieval scope only (not displayed in UI, documents only).
    #[serde(default)]
    pub rag_attachment_ids: Vec<uuid::Uuid>,
    /// Web search configuration.
    #[serde(default)]
    pub web_search: Option<WebSearchConfig>,
}

impl modkit::api::api_dto::RequestApiDto for StreamMessageRequest {}

/// Web search toggle.
#[derive(Debug, Clone, serde::Deserialize, ToSchema)]
pub struct WebSearchConfig {
    pub enabled: bool,
}

// ── Attachment DTOs ──

/// Structured thumbnail response.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct ThumbnailDto {
    pub content_type: String,
    pub width: i32,
    pub height: i32,
    pub data_base64: String,
}

/// Full attachment metadata response.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct AttachmentResponseDto {
    #[serde(rename = "attachment_id")]
    pub id: Uuid,
    pub status: String,
    pub kind: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub img_thumbnail: Option<ThumbnailDto>,
    /// Present only when status == "failed".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub summary_updated_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<Attachment> for AttachmentResponseDto {
    fn from(a: Attachment) -> Self {
        let error_code = if a.status == AttachmentStatus::Failed {
            a.error_code
        } else {
            None
        };

        // DESIGN: img_thumbnail present only when status=ready AND kind=image.
        let img_thumbnail =
            if a.status == AttachmentStatus::Ready && a.kind == AttachmentKind::Image {
                a.img_thumbnail.map(|t| ThumbnailDto {
                    content_type: "image/webp".to_owned(),
                    width: t.width,
                    height: t.height,
                    data_base64: BASE64.encode(&t.data),
                })
            } else {
                None
            };

        Self {
            id: a.id,
            status: a.status.as_str().to_owned(),
            kind: a.kind.as_str().to_owned(),
            filename: a.filename,
            content_type: a.content_type,
            size_bytes: a.size_bytes,
            doc_summary: a.doc_summary,
            img_thumbnail,
            error_code,
            summary_updated_at: a.summary_updated_at,
            created_at: a.created_at,
        }
    }
}

/// Minimal upload response (201 Created).
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct UploadAttachmentResponseDto {
    #[serde(rename = "attachment_id")]
    pub id: Uuid,
    pub status: String,
}

impl UploadAttachmentResponseDto {
    #[must_use]
    pub fn pending(id: Uuid) -> Self {
        Self {
            id,
            status: AttachmentStatus::Pending.as_str().to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use mini_chat_sdk::models::{Attachment, AttachmentKind, AttachmentStatus, ThumbnailData};
    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::{AttachmentResponseDto, StreamMessageRequest, UploadAttachmentResponseDto};

    fn make_attachment(status: AttachmentStatus, kind: AttachmentKind) -> Attachment {
        Attachment {
            id: Uuid::nil(),
            chat_id: Uuid::nil(),
            filename: "test.pdf".to_owned(),
            content_type: "application/pdf".to_owned(),
            size_bytes: 1024,
            storage_backend: "azure".to_owned(),
            status,
            kind,
            doc_summary: None,
            img_thumbnail: None,
            error_code: None,
            summary_updated_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            deleted_at: None,
        }
    }

    #[test]
    fn ready_attachment_omits_error_code_in_json() {
        let attachment = make_attachment(AttachmentStatus::Ready, AttachmentKind::Document);
        let dto = AttachmentResponseDto::from(attachment);
        assert!(dto.error_code.is_none());
        let json = serde_json::to_string(&dto).unwrap();
        assert!(
            !json.contains("error_code"),
            "error_code should be absent from JSON"
        );
    }

    #[test]
    fn failed_attachment_includes_error_code() {
        let mut attachment = make_attachment(AttachmentStatus::Failed, AttachmentKind::Document);
        attachment.error_code = Some("provider_upload_failed".to_owned());
        let dto = AttachmentResponseDto::from(attachment);
        assert_eq!(dto.error_code.as_deref(), Some("provider_upload_failed"));
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("provider_upload_failed"));
    }

    #[test]
    fn pending_attachment_omits_error_code_even_if_set() {
        let mut attachment = make_attachment(AttachmentStatus::Pending, AttachmentKind::Document);
        attachment.error_code = Some("should_not_appear".to_owned());
        let dto = AttachmentResponseDto::from(attachment);
        assert!(dto.error_code.is_none());
    }

    #[test]
    fn thumbnail_encoded_as_base64_webp_object() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let raw_bytes = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let mut attachment = make_attachment(AttachmentStatus::Ready, AttachmentKind::Image);
        attachment.img_thumbnail = Some(ThumbnailData {
            data: raw_bytes.clone(),
            width: 64,
            height: 48,
        });
        let dto = AttachmentResponseDto::from(attachment);
        let thumb = dto.img_thumbnail.unwrap();
        assert_eq!(thumb.content_type, "image/webp");
        assert_eq!(thumb.width, 64);
        assert_eq!(thumb.height, 48);
        let decoded = STANDARD.decode(&thumb.data_base64).unwrap();
        assert_eq!(decoded, raw_bytes);
    }

    #[test]
    fn no_thumbnail_omits_field_in_json() {
        let attachment = make_attachment(AttachmentStatus::Ready, AttachmentKind::Document);
        let dto = AttachmentResponseDto::from(attachment);
        assert!(dto.img_thumbnail.is_none());
        let json = serde_json::to_string(&dto).unwrap();
        assert!(
            !json.contains("img_thumbnail"),
            "thumbnail should be absent"
        );
    }

    #[test]
    fn provider_file_id_not_in_dto_fields() {
        let attachment = make_attachment(AttachmentStatus::Ready, AttachmentKind::Document);
        let json = serde_json::to_string(&AttachmentResponseDto::from(attachment)).unwrap();
        assert!(
            !json.contains("provider_file_id"),
            "provider_file_id must never appear in API response"
        );
    }

    #[test]
    fn upload_response_pending_factory() {
        let id = Uuid::new_v4();
        let dto = UploadAttachmentResponseDto::pending(id);
        assert_eq!(dto.id, id);
        assert_eq!(dto.status, "pending");
    }

    #[test]
    fn thumbnail_suppressed_for_pending_image() {
        let mut attachment = make_attachment(AttachmentStatus::Pending, AttachmentKind::Image);
        attachment.img_thumbnail = Some(ThumbnailData {
            data: vec![0xFF],
            width: 10,
            height: 10,
        });
        let dto = AttachmentResponseDto::from(attachment);
        assert!(
            dto.img_thumbnail.is_none(),
            "thumbnail must be suppressed when status != ready"
        );
    }

    #[test]
    fn thumbnail_suppressed_for_failed_image() {
        let mut attachment = make_attachment(AttachmentStatus::Failed, AttachmentKind::Image);
        attachment.error_code = Some("processing_failed".to_owned());
        attachment.img_thumbnail = Some(ThumbnailData {
            data: vec![0xFF],
            width: 10,
            height: 10,
        });
        let dto = AttachmentResponseDto::from(attachment);
        assert!(
            dto.img_thumbnail.is_none(),
            "thumbnail must be suppressed when status == failed"
        );
    }

    #[test]
    fn thumbnail_suppressed_for_ready_document() {
        let mut attachment = make_attachment(AttachmentStatus::Ready, AttachmentKind::Document);
        attachment.img_thumbnail = Some(ThumbnailData {
            data: vec![0xFF],
            width: 10,
            height: 10,
        });
        let dto = AttachmentResponseDto::from(attachment);
        assert!(
            dto.img_thumbnail.is_none(),
            "thumbnail must be suppressed when kind != image"
        );
    }

    #[test]
    fn doc_summary_absent_when_none() {
        let attachment = make_attachment(AttachmentStatus::Ready, AttachmentKind::Document);
        let json = serde_json::to_string(&AttachmentResponseDto::from(attachment)).unwrap();
        assert!(!json.contains("doc_summary"));
    }

    #[test]
    fn doc_summary_present_when_set() {
        let mut attachment = make_attachment(AttachmentStatus::Ready, AttachmentKind::Document);
        attachment.doc_summary = Some("A brief summary.".to_owned());
        let json = serde_json::to_string(&AttachmentResponseDto::from(attachment)).unwrap();
        assert!(json.contains("A brief summary."));
    }

    // ── StreamMessageRequest deserialization ─────────────────────────────

    #[test]
    fn deserialize_stream_request_with_rag_attachment_ids() {
        let json = r#"{
            "content": "hello",
            "attachment_ids": ["00000000-0000-0000-0000-000000000001"],
            "rag_attachment_ids": ["00000000-0000-0000-0000-000000000002"]
        }"#;
        let req: StreamMessageRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.content, "hello");
        assert_eq!(req.attachment_ids.len(), 1);
        assert_eq!(req.rag_attachment_ids.len(), 1);
    }

    #[test]
    fn deserialize_stream_request_without_rag_attachment_ids() {
        let json = r#"{"content": "hello"}"#;
        let req: StreamMessageRequest = serde_json::from_str(json).unwrap();
        assert!(req.rag_attachment_ids.is_empty());
        assert!(req.attachment_ids.is_empty());
    }
}
