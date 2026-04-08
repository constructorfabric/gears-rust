use modkit_macros::domain_model;
use time::OffsetDateTime;
use uuid::Uuid;

// ── Chat ──

/// A chat conversation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chat {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub model: String,
    pub title: Option<String>,
    pub is_temporary: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Enriched chat response with message count (no `tenant_id/user_id`).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatDetail {
    pub id: Uuid,
    pub model: String,
    pub title: Option<String>,
    pub is_temporary: bool,
    pub message_count: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Data for creating a new chat.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewChat {
    pub model: Option<String>,
    pub title: Option<String>,
    pub is_temporary: bool,
}

/// Partial update data for a chat.
///
/// Uses `Option<Option<String>>` for nullable fields to distinguish
/// "not provided" (None) from "set to null" (Some(None)).
///
/// Note: `model` is immutable for the chat lifetime
/// (`cpt-cf-mini-chat-constraint-model-locked-per-chat`).
/// `is_temporary` toggling is a P2 feature (`:temporary` endpoint).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(clippy::option_option)]
pub struct ChatPatch {
    pub title: Option<Option<String>>,
}

// ── Message ──

/// A chat message as returned by the list endpoint.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: Uuid,
    pub request_id: Uuid,
    pub role: String,
    pub content: String,
    pub attachments: Vec<AttachmentSummary>,
    pub my_reaction: Option<ReactionKind>,
    pub model: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub created_at: OffsetDateTime,
}

/// Lightweight attachment metadata embedded in Message objects.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSummary {
    pub attachment_id: Uuid,
    pub kind: String,
    pub filename: String,
    pub status: String,
    pub img_thumbnail: Option<ImgThumbnail>,
}

/// Server-generated preview thumbnail for an image attachment.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImgThumbnail {
    pub content_type: String,
    pub width: i32,
    pub height: i32,
    pub data_base64: String,
}

// ── Reaction ──

/// Binary like/dislike reaction value.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionKind {
    Like,
    Dislike,
}

impl ReactionKind {
    /// Parse from a string value ("like" / "dislike").
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "like" => Some(Self::Like),
            "dislike" => Some(Self::Dislike),
            _ => None,
        }
    }

    /// Wire representation used in DB and REST.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Like => "like",
            Self::Dislike => "dislike",
        }
    }
}

impl std::fmt::Display for ReactionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A reaction on an assistant message.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    pub message_id: Uuid,
    pub kind: ReactionKind,
    pub created_at: OffsetDateTime,
}

// ── Model Catalog (resolved projection) ──

/// A model resolved from the policy catalog for the current user.
///
/// Combines the public display projection with internal routing
/// metadata (`provider_model_id`, `provider_id`) needed for LLM
/// API requests. The DTO layer controls which fields are exposed
/// over the wire.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub model_id: String,
    /// Provider-side model ID (e.g. `"gpt-5.2"`, `"claude-opus-4-6"`). Sent in LLM API requests.
    pub provider_model_id: String,
    /// Maps to a key in `MiniChatConfig.providers` (e.g. `"openai"`, `"azure_openai"`).
    pub provider_id: String,
    pub display_name: String,
    pub tier: String,
    pub multiplier_display: String,
    pub description: Option<String>,
    pub multimodal_capabilities: Vec<String>,
    pub context_window: u32,
    /// Per-model upload file size limit in MB, from CCM policy snapshot.
    /// Used as the provider-side ceiling in two-layer limit resolution
    /// (`min(ConfigMap, CCM)`).
    pub max_file_size_mb: u32,
    /// System prompt sent as `instructions` in every LLM request for this model.
    /// Sourced from `ModelCatalogEntry.system_prompt` (per-model, per-policy-version).
    pub system_prompt: String,
    /// Tool support flags captured at resolution time.
    pub tool_support: mini_chat_sdk::ModelToolSupport,
}

impl From<&mini_chat_sdk::ModelCatalogEntry> for ResolvedModel {
    fn from(e: &mini_chat_sdk::ModelCatalogEntry) -> Self {
        Self {
            model_id: e.model_id.clone(),
            provider_model_id: e.provider_model_id.clone(),
            provider_id: e.provider_id.clone(),
            display_name: e.display_name.clone(),
            tier: match e.tier {
                mini_chat_sdk::ModelTier::Standard => "standard".to_owned(),
                mini_chat_sdk::ModelTier::Premium => "premium".to_owned(),
            },
            multiplier_display: e.multiplier_display.clone(),
            description: if e.description.is_empty() {
                None
            } else {
                Some(e.description.clone())
            },
            multimodal_capabilities: e.multimodal_capabilities.clone(),
            context_window: e.context_window,
            max_file_size_mb: e.general_config.max_file_size_mb,
            system_prompt: e.system_prompt.clone(),
            tool_support: e.general_config.tool_support.clone(),
        }
    }
}
