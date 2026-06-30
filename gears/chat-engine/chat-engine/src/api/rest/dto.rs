//! Wire-format DTOs for the Chat Engine REST surface.
//!
//! These are the only types that derive `utoipa::ToSchema` — domain models
//! in `domain/` and SDK models in `chat_engine_sdk::models` stay
//! transport-agnostic. Each DTO carries the exact field names / optionality
//! locked by `modules/chat-engine/api/http-protocol.json` and the SDK wire
//! formats sealed in Phase 5.
//!
//! The streaming event DTOs ([`StreamingStartDto`], [`StreamingChunkDto`],
//! [`StreamingCompleteDto`], [`StreamingErrorDto`]) and the discriminated
//! [`StreamingEventDto`] union are the canonical wire shapes for the NDJSON
//! response bodies. They are intentionally **flat** — `chunk` is a
//! `String`, `error` is a `String` — per §1.5 of
//! `docs/features/plugin-system.md`.
//
// @cpt-cf-chat-engine-api-dto:p14
// @cpt-cf-chat-engine-adr-http-client-protocol:p14

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use toolkit_macros::api_dto;
use utoipa::ToSchema;
use uuid::Uuid;

use chat_engine_sdk::models::{
    CapabilityValue, LifecycleState, Message as SdkMessage, MessagePart as SdkMessagePart,
    MessagePartInput as SdkMessagePartInput, MessagePartType, MessageRole, Session as SdkSession,
    SessionType as SdkSessionType, VariantInfo,
};

// ---------------------------------------------------------------------------
// Session DTOs
// ---------------------------------------------------------------------------

/// Wire-shape projection of [`SdkSession`]. The bearer `share_token` is
/// intentionally redacted from list / get responses; the only sanctioned
/// surface for the raw token is the dedicated share endpoint.
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct SessionDto {
    pub session_id: Uuid,
    pub tenant_id: String,
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_type_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_capabilities: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
    pub lifecycle_state: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

impl From<SdkSession> for SessionDto {
    fn from(s: SdkSession) -> Self {
        Self {
            session_id: s.session_id,
            tenant_id: s.tenant_id.to_string(),
            user_id: s.user_id.to_string(),
            client_id: s.client_id,
            session_type_id: s.session_type_id,
            enabled_capabilities: s.enabled_capabilities,
            metadata: s.metadata,
            lifecycle_state: s.lifecycle_state.as_str().to_owned(),
            created_at: s.created_at,
            updated_at: s.updated_at,
        }
    }
}

/// Body for `POST /chat-engine/v1/sessions`.
#[api_dto(request)]
#[derive(Debug, Clone)]
pub struct CreateSessionRequestDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_type_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
}

/// Body for `POST /chat-engine/v1/sessions/{id}/switch-type`.
#[api_dto(request)]
#[derive(Debug, Clone)]
pub struct SwitchSessionTypeRequestDto {
    pub session_type_id: Uuid,
}

/// Cursor-paginated list envelope for `GET /chat-engine/v1/sessions`.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct SessionListDto {
    pub items: Vec<SessionDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

// ---------------------------------------------------------------------------
// SessionType DTOs
// ---------------------------------------------------------------------------

/// Body for `POST /chat-engine/v1/session-types`.
#[api_dto(request)]
#[derive(Debug, Clone)]
pub struct RegisterSessionTypeRequestDto {
    /// Human-readable name (opaque to Chat Engine).
    pub name: String,
    /// GTS plugin instance ID to bind. `None` registers an unwired type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_instance_id: Option<String>,
    /// Opaque plugin configuration JSON handed to the bound plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_config: Option<JsonValue>,
}

/// Wire-shape projection of [`SdkSessionType`].
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct SessionTypeDto {
    pub session_type_id: Uuid,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_instance_id: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

impl From<SdkSessionType> for SessionTypeDto {
    fn from(t: SdkSessionType) -> Self {
        Self {
            session_type_id: t.session_type_id,
            name: t.name,
            plugin_instance_id: t.plugin_instance_id,
            created_at: t.created_at,
            updated_at: t.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Message DTOs
// ---------------------------------------------------------------------------

/// Wire-shape of a persisted message part (FR-022).
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct MessagePartDto {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub part_type: String,
    pub content: JsonValue,
    pub number: u32,
    /// Document citations attached to a `text` part (FR-023), serialized
    /// verbatim. Omitted from the wire when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_citations: Vec<JsonValue>,
    /// Web-page citations attached to a `text` part. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_citations: Vec<JsonValue>,
    /// URL references attached to a `text` part. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<JsonValue>,
}

impl From<SdkMessagePart> for MessagePartDto {
    fn from(p: SdkMessagePart) -> Self {
        Self {
            id: p.id,
            part_type: part_type_to_wire(p.part_type).to_owned(),
            content: p.content,
            number: p.number,
            file_citations: p
                .file_citations
                .iter()
                .filter_map(|c| serde_json::to_value(c).ok())
                .collect(),
            link_citations: p
                .link_citations
                .iter()
                .filter_map(|c| serde_json::to_value(c).ok())
                .collect(),
            references: p
                .references
                .iter()
                .filter_map(|r| serde_json::to_value(r).ok())
                .collect(),
        }
    }
}

/// Wire-shape of an inbound message part (no `id`/`number`; assigned on persist).
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct MessagePartInputDto {
    #[serde(rename = "type")]
    pub part_type: String,
    pub content: JsonValue,
}

impl From<MessagePartInputDto> for SdkMessagePartInput {
    fn from(d: MessagePartInputDto) -> Self {
        let part_type = part_type_from_wire(&d.part_type);
        Self {
            content: normalize_part_content(part_type, d.content),
            part_type,
            // Citations/references are plugin-produced (assistant side); the
            // inbound user-message wire shape does not carry them.
            file_citations: Vec::new(),
            link_citations: Vec::new(),
            references: Vec::new(),
        }
    }
}

/// Canonicalize inbound part content to the persisted/streamed wire shape so
/// user and assistant parts of the same type serialize identically.
///
/// A `text` part's canonical content is the object `{ "text": "<body>" }`
/// (see [`MessagePart::text`](chat_engine_sdk::models::MessagePart::text) and
/// the streamed assistant form). Clients commonly send a bare JSON string for
/// convenience; wrap it so it matches the assistant side. Already-canonical
/// objects and every other part type pass through untouched — per-type shape
/// validation stays in the service/plugin layer.
fn normalize_part_content(part_type: MessagePartType, content: JsonValue) -> JsonValue {
    match (part_type, content) {
        (MessagePartType::Text, JsonValue::String(text)) => {
            serde_json::json!({ "text": text })
        }
        (_, content) => content,
    }
}

/// Total, lossless mapping of a part type to its wire string.
fn part_type_to_wire(t: MessagePartType) -> &'static str {
    match t {
        MessagePartType::Text => "text",
        MessagePartType::Code => "code",
        MessagePartType::Images => "images",
        MessagePartType::Videos => "videos",
        MessagePartType::Links => "links",
        MessagePartType::Statuses => "statuses",
    }
}

/// Parse a wire part-type string, defaulting unknown values to `text` so a
/// malformed body never panics. Strict validation lives in the service layer.
fn part_type_from_wire(s: &str) -> MessagePartType {
    match s {
        "code" => MessagePartType::Code,
        "images" => MessagePartType::Images,
        "videos" => MessagePartType::Videos,
        "links" => MessagePartType::Links,
        "statuses" => MessagePartType::Statuses,
        _ => MessagePartType::Text,
    }
}

/// Convenience: convert wire parts to the SDK input shape.
#[must_use]
pub fn parts_into_sdk(parts: Vec<MessagePartInputDto>) -> Vec<SdkMessagePartInput> {
    parts.into_iter().map(SdkMessagePartInput::from).collect()
}

/// Wire-shape projection of [`SdkMessage`].
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct MessageDto {
    pub message_id: Uuid,
    pub session_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_message_id: Option<Uuid>,
    pub variant_index: u32,
    pub is_active: bool,
    pub role: String,
    #[serde(default)]
    pub parts: Vec<MessagePartDto>,
    #[serde(default)]
    pub file_ids: Vec<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
    pub is_complete: bool,
    pub is_hidden_from_user: bool,
    pub is_hidden_from_backend: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

impl From<SdkMessage> for MessageDto {
    fn from(m: SdkMessage) -> Self {
        Self {
            message_id: m.message_id,
            session_id: m.session_id,
            parent_message_id: m.parent_message_id,
            variant_index: m.variant_index,
            is_active: m.is_active,
            role: role_to_wire(&m.role).to_owned(),
            parts: m.parts.into_iter().map(MessagePartDto::from).collect(),
            file_ids: m.file_ids,
            metadata: m.metadata,
            is_complete: m.is_complete,
            is_hidden_from_user: m.is_hidden_from_user,
            is_hidden_from_backend: m.is_hidden_from_backend,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

fn role_to_wire(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
    }
}

/// Body for `POST /chat-engine/v1/sessions/{id}/messages`.
#[api_dto(request)]
#[derive(Debug, Clone)]
pub struct SendMessageRequestDto {
    #[serde(default)]
    pub parts: Vec<MessagePartInputDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_ids: Option<Vec<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_message_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<CapabilityValueDto>>,
}

/// Body for `POST /chat-engine/v1/messages/{id}/recreate`.
#[api_dto(request)]
#[derive(Debug, Clone, Default)]
pub struct RecreateMessageRequestDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_capabilities: Option<Vec<CapabilityValueDto>>,
}

/// Wire projection of [`CapabilityValue`].
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct CapabilityValueDto {
    pub name: String,
    pub value: JsonValue,
}

impl From<CapabilityValueDto> for CapabilityValue {
    fn from(v: CapabilityValueDto) -> Self {
        Self {
            name: v.name,
            value: v.value,
        }
    }
}

impl From<CapabilityValue> for CapabilityValueDto {
    fn from(v: CapabilityValue) -> Self {
        Self {
            name: v.name,
            value: v.value,
        }
    }
}

/// Convenience: convert a wire-typed `Vec<CapabilityValueDto>` to the SDK
/// shape used by the service layer.
#[must_use]
pub fn capabilities_into_sdk(values: Vec<CapabilityValueDto>) -> Vec<CapabilityValue> {
    values.into_iter().map(CapabilityValue::from).collect()
}

/// List response envelope for `GET /chat-engine/v1/sessions/{id}/messages`.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct MessageListDto {
    pub items: Vec<MessageDto>,
}

// ---------------------------------------------------------------------------
// Variants
// ---------------------------------------------------------------------------

/// Wire projection of [`VariantInfo`].
#[api_dto(request, response)]
#[derive(Debug, Clone)]
pub struct VariantInfoDto {
    pub message_id: Uuid,
    pub variant_index: u32,
    pub total_variants: u32,
    pub is_active: bool,
}

impl From<VariantInfo> for VariantInfoDto {
    fn from(v: VariantInfo) -> Self {
        Self {
            message_id: v.message_id,
            variant_index: v.variant_index,
            total_variants: v.total_variants,
            is_active: v.is_active,
        }
    }
}

/// `GET /chat-engine/v1/messages/{id}/variants` response envelope.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct VariantListDto {
    pub variants: Vec<VariantInfoDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_index: Option<u32>,
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// Body for `POST /chat-engine/v1/sessions/{id}/search` and
/// `POST /chat-engine/v1/sessions/search`.
#[api_dto(request)]
#[derive(Debug, Clone)]
pub struct SearchRequestDto {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

/// Response shape for search endpoints.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct SearchResultsDto {
    pub results: Vec<JsonValue>,
}

// ---------------------------------------------------------------------------
// Reactions
// ---------------------------------------------------------------------------

/// Body for `POST /chat-engine/v1/messages/{id}/reactions`.
#[api_dto(request)]
#[derive(Debug, Clone)]
pub struct ReactionRequestDto {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<JsonValue>,
}

/// Single reaction record.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ReactionDto {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<JsonValue>,
    pub user_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
}

/// `GET /chat-engine/v1/messages/{id}/reactions` envelope.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ReactionListDto {
    pub reactions: Vec<ReactionDto>,
}

// ---------------------------------------------------------------------------
// Export / Share
// ---------------------------------------------------------------------------

/// Body for `POST /chat-engine/v1/sessions/{id}/export`.
#[api_dto(request)]
#[derive(Debug, Clone, Default)]
pub struct ExportRequestDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_plugin_metadata: Option<bool>,
}

/// Response for `POST /chat-engine/v1/sessions/{id}/export`.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ExportAcceptedDto {
    pub session_id: Uuid,
    pub format: String,
    pub download_url: String,
    pub message_count: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: time::OffsetDateTime,
}

/// Body for `POST /chat-engine/v1/sessions/{id}/share`.
#[api_dto(request)]
#[derive(Debug, Clone, Default)]
pub struct ShareRequestDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_hours: Option<u32>,
}

/// Response for `POST /chat-engine/v1/sessions/{id}/share`.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct ShareResponseDto {
    pub session_id: Uuid,
    pub share_token: String,
    pub share_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "rfc3339_opt")]
    pub expires_at: Option<time::OffsetDateTime>,
}

/// Response for `POST /chat-engine/v1/shared/{share_token}`.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct SharedSessionDto {
    pub session_id: Uuid,
    pub messages: Vec<MessageDto>,
    pub read_only: bool,
}

// ---------------------------------------------------------------------------
// Summarization
// ---------------------------------------------------------------------------

/// `POST /chat-engine/v1/sessions/{id}/summarize` accepted-envelope.
#[api_dto(response)]
#[derive(Debug, Clone)]
pub struct SummarizeAcceptedDto {
    pub session_id: Uuid,
    pub status_url: String,
}

// ---------------------------------------------------------------------------
// Streaming wire events (NDJSON)
// ---------------------------------------------------------------------------

/// `event: "start"` — begin streaming.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StreamingStartDto {
    pub message_id: Uuid,
}

/// `event: "chunk"` — append `chunk` (text fragment) to the assistant
/// message body. `chunk` is intentionally `String`, NOT a structured
/// payload.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StreamingChunkDto {
    pub message_id: Uuid,
    pub chunk: String,
}

/// `event: "complete"` — streaming finished successfully. `metadata` is a
/// plugin-defined object; it is OMITTED from the wire payload when
/// absent.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StreamingCompleteDto {
    pub message_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
}

/// `event: "error"` — terminal streaming error. `error` is a single
/// human-readable string. Discriminator prefixes (`context_overflow:`,
/// `stream_interrupted:`, `deadline_exceeded:`) are surfaced verbatim per
/// ADR-0023. No further events follow.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StreamingErrorDto {
    pub message_id: Uuid,
    pub error: String,
}

/// Tagged-union of all streaming events. NDJSON serialization writes one
/// `StreamingEventDto` per line. The discriminator field is `type` per
/// the OpenAPI spec (`api/http-protocol.json`) — see
/// `StreamingStartEvent.type`, `StreamingChunkEvent.type`, …
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamingEventDto {
    Start(StreamingStartDto),
    Chunk(StreamingChunkDto),
    Complete(StreamingCompleteDto),
    Error(StreamingErrorDto),
}

// Mark the streaming DTOs as `ResponseApiDto` so they can appear in
// `OperationBuilder::json_response_with_schema`.
impl toolkit::api::api_dto::ResponseApiDto for StreamingStartDto {}
impl toolkit::api::api_dto::ResponseApiDto for StreamingChunkDto {}
impl toolkit::api::api_dto::ResponseApiDto for StreamingCompleteDto {}
impl toolkit::api::api_dto::ResponseApiDto for StreamingErrorDto {}
impl toolkit::api::api_dto::ResponseApiDto for StreamingEventDto {}

// ---------------------------------------------------------------------------
// Domain → DTO conversions for the streaming events
// ---------------------------------------------------------------------------

use crate::domain::message::{
    StreamingChunkEvent, StreamingCompleteEvent, StreamingErrorEvent, StreamingStartEvent,
};

impl From<StreamingStartEvent> for StreamingStartDto {
    fn from(e: StreamingStartEvent) -> Self {
        Self {
            message_id: e.message_id,
        }
    }
}

impl From<StreamingChunkEvent> for StreamingChunkDto {
    fn from(e: StreamingChunkEvent) -> Self {
        Self {
            message_id: e.message_id,
            chunk: e.chunk,
        }
    }
}

impl From<StreamingCompleteEvent> for StreamingCompleteDto {
    fn from(e: StreamingCompleteEvent) -> Self {
        Self {
            message_id: e.message_id,
            metadata: e.metadata,
        }
    }
}

impl From<StreamingErrorEvent> for StreamingErrorDto {
    fn from(e: StreamingErrorEvent) -> Self {
        Self {
            message_id: e.message_id,
            error: e.error,
        }
    }
}

// ---------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------

/// Convert a `LifecycleState` into its lowercase wire representation.
#[must_use]
pub fn lifecycle_to_wire(state: LifecycleState) -> &'static str {
    state.as_str()
}

mod rfc3339_opt {
    use serde::Serializer;
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    // `&Option<_>` is mandated by serde's `serialize_with` signature contract.
    #[allow(clippy::ref_option)]
    pub fn serialize<S>(value: &Option<OffsetDateTime>, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(dt) => {
                let s = dt.format(&Rfc3339).map_err(serde::ser::Error::custom)?;
                ser.serialize_str(&s)
            }
            None => ser.serialize_none(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_part_input_string_is_canonicalized_to_object() {
        // A bare-string text part is wrapped so it matches the persisted /
        // streamed assistant shape `{ "text": ... }`.
        let dto = MessagePartInputDto {
            part_type: "text".into(),
            content: json!("hello"),
        };
        let sdk = SdkMessagePartInput::from(dto);
        assert_eq!(sdk.part_type, MessagePartType::Text);
        assert_eq!(sdk.content, json!({ "text": "hello" }));
    }

    #[test]
    fn text_part_input_object_passes_through() {
        let dto = MessagePartInputDto {
            part_type: "text".into(),
            content: json!({ "text": "hi" }),
        };
        assert_eq!(
            SdkMessagePartInput::from(dto).content,
            json!({ "text": "hi" })
        );
    }

    #[test]
    fn non_text_part_content_is_untouched() {
        let dto = MessagePartInputDto {
            part_type: "links".into(),
            content: json!({ "links": [{ "url": "https://e.com" }] }),
        };
        let sdk = SdkMessagePartInput::from(dto);
        assert_eq!(sdk.part_type, MessagePartType::Links);
        assert_eq!(
            sdk.content,
            json!({ "links": [{ "url": "https://e.com" }] })
        );
    }

    #[test]
    fn streaming_event_dto_serializes_as_tagged_union() {
        let evt = StreamingEventDto::Start(StreamingStartDto {
            message_id: Uuid::nil(),
        });
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"type\":\"start\""));
    }

    #[test]
    fn streaming_chunk_dto_uses_flat_string_chunk() {
        let evt = StreamingEventDto::Chunk(StreamingChunkDto {
            message_id: Uuid::nil(),
            chunk: "hi".into(),
        });
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"type\":\"chunk\""));
        assert!(s.contains("\"chunk\":\"hi\""));
    }

    #[test]
    fn streaming_error_dto_uses_single_string_error_field() {
        let evt = StreamingEventDto::Error(StreamingErrorDto {
            message_id: Uuid::nil(),
            error: "context_overflow: too many tokens".into(),
        });
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"type\":\"error\""));
        assert!(s.contains("\"error\":\"context_overflow: too many tokens\""));
    }

    #[test]
    fn streaming_complete_dto_omits_metadata_when_none() {
        let evt = StreamingEventDto::Complete(StreamingCompleteDto {
            message_id: Uuid::nil(),
            metadata: None,
        });
        let value: serde_json::Value = serde_json::to_value(&evt).unwrap();
        assert!(
            value.get("metadata").is_none(),
            "metadata must be omitted when None"
        );
    }

    #[test]
    fn streaming_complete_dto_keeps_metadata_when_present() {
        let evt = StreamingEventDto::Complete(StreamingCompleteDto {
            message_id: Uuid::nil(),
            metadata: Some(json!({"model": "gpt-x"})),
        });
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"metadata\""));
        assert!(s.contains("\"model\":\"gpt-x\""));
    }

    #[test]
    fn session_dto_redacts_share_token() {
        let dto_json = serde_json::to_string(&SessionDto {
            session_id: Uuid::nil(),
            tenant_id: "t".into(),
            user_id: "u".into(),
            client_id: None,
            session_type_id: None,
            enabled_capabilities: None,
            metadata: None,
            lifecycle_state: "active".into(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
        })
        .unwrap();
        assert!(
            !dto_json.contains("share_token"),
            "share_token must never leak via SessionDto"
        );
    }
}
