//! HTTP DTOs (serde/utoipa) — REST-only request and response types.
//!
//! All REST DTOs live here; SDK `models.rs` stays transport-agnostic.
//! Provide `From` conversions between SDK models and DTOs in this file.
//!
//! Stream event types live in `domain::stream_events`; SSE wire conversion
//! and ordering enforcement live in `api::rest::sse`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::domain::models::{AttachmentSummary, ChatDetail, ImgThumbnail};
use crate::infra::db::entity::attachment::Model as AttachmentModel;
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
    pub my_reaction: Option<String>,
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
            my_reaction: m.my_reaction.map(|r| r.as_str().to_owned()),
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

/// Full attachment details returned by the GET attachment endpoint.
#[derive(Debug, Clone)]
#[modkit_macros::api_dto(response)]
pub struct AttachmentDetailDto {
    pub id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub status: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub img_thumbnail: Option<ImgThumbnailDto>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub summary_updated_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<AttachmentModel> for AttachmentDetailDto {
    fn from(m: AttachmentModel) -> Self {
        let img_thumbnail = m
            .img_thumbnail
            .zip(m.img_thumbnail_width)
            .zip(m.img_thumbnail_height)
            .map(|((bytes, w), h)| ImgThumbnailDto {
                content_type: "image/webp".to_owned(),
                width: w,
                height: h,
                data_base64: BASE64.encode(&bytes),
            });

        Self {
            id: m.id,
            filename: m.filename,
            content_type: m.content_type,
            size_bytes: m.size_bytes,
            status: m.status.to_string(),
            kind: m.attachment_kind.to_string(),
            error_code: m.error_code,
            doc_summary: m.doc_summary,
            img_thumbnail,
            summary_updated_at: m.summary_updated_at,
            created_at: m.created_at,
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
    /// Attachment IDs to include.
    #[serde(default)]
    pub attachment_ids: Vec<uuid::Uuid>,
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
