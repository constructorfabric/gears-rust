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
#[toolkit_macros::api_dto(request)]
pub struct CreateChatReq {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Request DTO for updating a chat title.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct UpdateChatReq {
    pub title: String,
}

/// Response DTO for chat details.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
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
///
/// Aliased to `MiniChatMessageDto` in the `OpenAPI`` schema: `chat-engine` also
/// exposes a `MessageDto`, and both gears register into the same api-gateway
/// `OpenAPI`` registry, so the bare ident would collide in `components.schemas`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
#[schema(as = MiniChatMessageDto)]
pub struct MessageDto {
    pub id: Uuid,
    pub request_id: Uuid,
    pub role: MessageRoleDto,
    pub content: String,
    pub attachments: Vec<AttachmentSummaryDto>,
    /// The caller's reaction to this message; `null` when there is none.
    #[schema(required)]
    pub my_reaction: Option<ReactionKindDto>,
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
            role: MessageRoleDto::from_db(&m.role),
            content: m.content,
            attachments: m
                .attachments
                .into_iter()
                .map(AttachmentSummaryDto::from)
                .collect(),
            my_reaction: m.my_reaction.map(ReactionKindDto::from),
            model: m.model,
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            created_at: m.created_at,
        }
    }
}

/// Message author role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum MessageRoleDto {
    User,
    Assistant,
    System,
}

impl MessageRoleDto {
    /// Map the stored role (`user` / `assistant` / `system`, enforced by the
    /// `MessageRole` entity enum).
    fn from_db(role: &str) -> Self {
        match role {
            "assistant" => Self::Assistant,
            "system" => Self::System,
            "user" => Self::User,
            other => {
                tracing::warn!(
                    role = other,
                    "unexpected stored message role; reported as user"
                );
                Self::User
            }
        }
    }
}

/// Reaction value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum ReactionKindDto {
    Like,
    Dislike,
}

impl From<crate::domain::models::ReactionKind> for ReactionKindDto {
    fn from(k: crate::domain::models::ReactionKind) -> Self {
        use crate::domain::models::ReactionKind;
        match k {
            ReactionKind::Like => Self::Like,
            ReactionKind::Dislike => Self::Dislike,
        }
    }
}

/// Lightweight attachment metadata embedded in Message responses.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct AttachmentSummaryDto {
    pub attachment_id: Uuid,
    pub kind: AttachmentKindDto,
    pub filename: String,
    pub status: AttachmentStatusDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub img_thumbnail: Option<ImgThumbnailDto>,
}

impl From<AttachmentSummary> for AttachmentSummaryDto {
    fn from(a: AttachmentSummary) -> Self {
        Self {
            attachment_id: a.attachment_id,
            kind: AttachmentKindDto::from_db(&a.kind),
            filename: a.filename,
            status: AttachmentStatusDto::from_db(&a.status),
            img_thumbnail: a.img_thumbnail.map(ImgThumbnailDto::from),
        }
    }
}

/// Server-generated preview thumbnail for an image attachment.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
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

/// Attachment lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum AttachmentStatusDto {
    Pending,
    Uploaded,
    Ready,
    Failed,
}

impl AttachmentStatusDto {
    /// Map the stored status string (DB check constraint).
    fn from_db(status: &str) -> Self {
        match status {
            "pending" => Self::Pending,
            "uploaded" => Self::Uploaded,
            "ready" => Self::Ready,
            "failed" => Self::Failed,
            other => {
                tracing::warn!(
                    status = other,
                    "unexpected stored attachment status; reported as failed"
                );
                Self::Failed
            }
        }
    }
}

impl From<crate::infra::db::entity::attachment::AttachmentStatus> for AttachmentStatusDto {
    fn from(s: crate::infra::db::entity::attachment::AttachmentStatus) -> Self {
        use crate::infra::db::entity::attachment::AttachmentStatus;
        match s {
            AttachmentStatus::Pending => Self::Pending,
            AttachmentStatus::Uploaded => Self::Uploaded,
            AttachmentStatus::Ready => Self::Ready,
            AttachmentStatus::Failed => Self::Failed,
        }
    }
}

/// Attachment kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum AttachmentKindDto {
    Document,
    Image,
}

impl AttachmentKindDto {
    /// Map the stored kind string (DB check constraint).
    fn from_db(kind: &str) -> Self {
        if kind == "image" {
            Self::Image
        } else {
            Self::Document
        }
    }
}

impl From<crate::infra::db::entity::attachment::AttachmentKind> for AttachmentKindDto {
    fn from(k: crate::infra::db::entity::attachment::AttachmentKind) -> Self {
        use crate::infra::db::entity::attachment::AttachmentKind;
        match k {
            AttachmentKind::Document => Self::Document,
            AttachmentKind::Image => Self::Image,
        }
    }
}

/// Full attachment details returned by the GET attachment endpoint.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct AttachmentDetailDto {
    pub id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub status: AttachmentStatusDto,
    pub kind: AttachmentKindDto,
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
            status: m.status.into(),
            kind: m.attachment_kind.into(),
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
#[toolkit_macros::api_dto(request)]
pub struct SetReactionReq {
    pub reaction: String,
}

/// Response DTO for a reaction.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
#[schema(as = MiniChatReactionDto)]
pub struct ReactionDto {
    pub message_id: Uuid,
    pub reaction: ReactionKindDto,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<crate::domain::models::Reaction> for ReactionDto {
    fn from(r: crate::domain::models::Reaction) -> Self {
        Self {
            message_id: r.message_id,
            reaction: r.kind.into(),
            created_at: r.created_at,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Model DTOs
// ════════════════════════════════════════════════════════════════════════════

/// Model tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub enum ModelTierDto {
    Standard,
    Premium,
}

/// Response DTO for a single model.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ModelDto {
    pub model_id: String,
    pub display_name: String,
    pub tier: ModelTierDto,
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
            tier: if m.tier == "premium" {
                ModelTierDto::Premium
            } else {
                ModelTierDto::Standard
            },
            multiplier_display: m.multiplier_display,
            description: m.description,
            multimodal_capabilities: m.multimodal_capabilities,
            context_window: m.context_window,
        }
    }
}

/// Response DTO for the model list endpoint.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
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
    /// Idempotency key: any UUID; generated by the server when omitted.
    #[serde(default)]
    pub request_id: Option<uuid::Uuid>,
    /// Attachment IDs to include.
    #[serde(default)]
    pub attachment_ids: Vec<uuid::Uuid>,
    /// Web search configuration.
    #[serde(default)]
    pub web_search: Option<WebSearchConfig>,
}

impl toolkit::api::api_dto::RequestApiDto for StreamMessageRequest {}

/// Web search toggle.
#[derive(Debug, Clone, serde::Deserialize, ToSchema)]
pub struct WebSearchConfig {
    pub enabled: bool,
}

// ════════════════════════════════════════════════════════════════════════════
// Turn DTOs
// ════════════════════════════════════════════════════════════════════════════

/// Turn state as reported by the turn status endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatusState {
    Running,
    Done,
    Error,
    Cancelled,
}

/// Response DTO for `GET /chats/{id}/turns/{request_id}`.
#[derive(Debug, serde::Serialize, ToSchema)]
pub struct TurnStatusResponse {
    pub request_id: Uuid,
    pub state: TurnStatusState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_message_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl toolkit::api::api_dto::ResponseApiDto for TurnStatusResponse {}

/// Request DTO for `PATCH /chats/{id}/turns/{request_id}` (edit).
#[derive(Debug, serde::Deserialize, ToSchema)]
pub struct EditTurnRequest {
    pub content: String,
}

impl toolkit::api::api_dto::RequestApiDto for EditTurnRequest {}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::models::ReactionKind;
    use crate::infra::db::entity::attachment::{AttachmentKind, AttachmentStatus};

    fn wire<T: serde::Serialize>(v: T) -> String {
        serde_json::to_value(v)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Stored strings map to the enum values the schema documents; the wire
    /// value equals the stored one.
    #[test]
    fn stored_strings_map_to_wire_enums() {
        for role in ["user", "assistant", "system"] {
            assert_eq!(wire(MessageRoleDto::from_db(role)), role);
        }
        for status in ["pending", "uploaded", "ready", "failed"] {
            assert_eq!(wire(AttachmentStatusDto::from_db(status)), status);
        }
        for kind in ["document", "image"] {
            assert_eq!(wire(AttachmentKindDto::from_db(kind)), kind);
        }
        // Values the DB constraints and entity enums rule out map to the
        // documented fallbacks.
        assert_eq!(wire(MessageRoleDto::from_db("bogus")), "user");
        assert_eq!(wire(AttachmentStatusDto::from_db("bogus")), "failed");
        assert_eq!(wire(AttachmentKindDto::from_db("bogus")), "document");
    }

    /// Turn status wire shape: optional fields are omitted when unset,
    /// `updated_at` is RFC 3339.
    #[test]
    fn turn_status_response_wire_shape() {
        let request_id = Uuid::nil();
        let running = TurnStatusResponse {
            request_id,
            state: TurnStatusState::Running,
            error_code: None,
            assistant_message_id: None,
            updated_at: time::macros::datetime!(2026-09-28 12:00:00 UTC),
        };
        assert_eq!(
            serde_json::to_value(&running).unwrap(),
            serde_json::json!({
                "request_id": request_id,
                "state": "running",
                "updated_at": "2026-09-28T12:00:00Z"
            })
        );
        let failed = TurnStatusResponse {
            state: TurnStatusState::Error,
            error_code: Some("provider_error".to_owned()),
            assistant_message_id: Some(request_id),
            ..running
        };
        let v = serde_json::to_value(&failed).unwrap();
        assert_eq!(v["state"], "error");
        assert_eq!(v["error_code"], "provider_error");
        assert_eq!(v["assistant_message_id"], serde_json::json!(request_id));
    }

    /// `content` is required in an edit request.
    #[test]
    fn edit_turn_request_requires_content() {
        let ok: EditTurnRequest =
            serde_json::from_value(serde_json::json!({"content": "x"})).unwrap();
        assert_eq!(ok.content, "x");
        assert!(serde_json::from_value::<EditTurnRequest>(serde_json::json!({})).is_err());
    }

    #[test]
    fn entity_enums_map_to_wire_enums() {
        assert_eq!(wire(ReactionKindDto::from(ReactionKind::Like)), "like");
        assert_eq!(
            wire(ReactionKindDto::from(ReactionKind::Dislike)),
            "dislike"
        );
        for (status, expected) in [
            (AttachmentStatus::Pending, "pending"),
            (AttachmentStatus::Uploaded, "uploaded"),
            (AttachmentStatus::Ready, "ready"),
            (AttachmentStatus::Failed, "failed"),
        ] {
            assert_eq!(wire(AttachmentStatusDto::from(status)), expected);
        }
        assert_eq!(
            wire(AttachmentKindDto::from(AttachmentKind::Document)),
            "document"
        );
        assert_eq!(
            wire(AttachmentKindDto::from(AttachmentKind::Image)),
            "image"
        );
    }
}
