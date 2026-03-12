use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

// ── Attachment ──

/// Attachment status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentStatus {
    Pending,
    Ready,
    Failed,
}

impl AttachmentStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "ready" => Some(Self::Ready),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Attachment kind derived from content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Document,
    Image,
}

impl AttachmentKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Image => "image",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "document" => Some(Self::Document),
            "image" => Some(Self::Image),
            _ => None,
        }
    }

    /// Derive kind from MIME content type.
    ///
    /// Normalises the input by trimming whitespace, lowercasing, and stripping
    /// any MIME parameters (everything after `;`) before matching.
    #[must_use]
    pub fn from_content_type(content_type: &str) -> Self {
        let normalized = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match normalized.as_str() {
            "image/png" | "image/jpeg" | "image/webp" => Self::Image,
            _ => Self::Document,
        }
    }
}

/// Thumbnail data stored in DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailData {
    pub data: Vec<u8>,
    pub width: i32,
    pub height: i32,
}

/// A file attachment (public-facing — never includes `provider_file_id`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub id: Uuid,
    pub chat_id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub storage_backend: String,
    pub status: AttachmentStatus,
    pub kind: AttachmentKind,
    pub doc_summary: Option<String>,
    pub img_thumbnail: Option<ThumbnailData>,
    pub error_code: Option<String>,
    pub summary_updated_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub deleted_at: Option<OffsetDateTime>,
}

/// Data for creating a new attachment.
#[derive(Debug, Clone)]
pub struct NewAttachment {
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub kind: AttachmentKind,
}

/// A chat vector store mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatVectorStore {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub vector_store_id: Option<String>,
    pub provider: String,
    pub file_count: i32,
    pub created_at: OffsetDateTime,
}

// ── Policy ──

/// Current policy version metadata for a user.
#[derive(Debug, Clone)]
pub struct PolicyVersionInfo {
    pub user_id: Uuid,
    pub policy_version: u64,
    pub generated_at: OffsetDateTime,
}

/// Full policy snapshot for a given version, including the model catalog
/// and kill switches (API: `PolicyByVersionResponse`).
#[derive(Debug, Clone)]
pub struct PolicySnapshot {
    pub user_id: Uuid,
    pub policy_version: u64,
    pub model_catalog: Vec<ModelCatalogEntry>,
    pub kill_switches: KillSwitches,
}

/// Tenant-level kill switches from the policy snapshot.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KillSwitches {
    pub disable_premium_tier: bool,
    pub force_standard_tier: bool,
    pub disable_web_search: bool,
    pub disable_file_search: bool,
    pub disable_images: bool,
}

/// A single model in the catalog (API: `PolicyModelCatalogItem`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalogEntry {
    /// Provider-level model identifier (e.g. "gpt-4").
    pub model_id: String,
    /// The model ID on the provider side (e.g., `"gpt-5.2"` for `OpenAI`,
    /// `"claude-opus-4-6"` for Anthropic). Sent in LLM API requests.
    pub provider_model_id: String,
    /// Display name shown in UI (may differ from `name`).
    pub display_name: String,
    /// Short description of the model.
    pub description: String,
    /// Model version string.
    pub version: String,
    /// LLM provider CTI identifier.
    pub provider_id: String,
    /// Routing identifier for provider resolution. Maps to a key in
    /// `MiniChatConfig.providers`. Values: `"openai"`, `"azure_openai"`.
    pub provider_display_name: String,
    /// URL to model icon.
    pub icon: String,
    /// Model tier (standard or premium).
    pub tier: ModelTier,
    pub enabled: bool,
    /// Multimodal capability flags, e.g. `VISION_INPUT`, `IMAGE_GENERATION`.
    pub multimodal_capabilities: Vec<String>,
    /// Maximum context window size in tokens.
    pub context_window: u32,
    /// Maximum output tokens the model can generate.
    pub max_output_tokens: u32,
    /// Maximum input tokens per request.
    pub max_input_tokens: u32,
    /// Credit multiplier for input tokens (micro-credits per 1000 tokens).
    pub input_tokens_credit_multiplier_micro: u64,
    /// Credit multiplier for output tokens (micro-credits per 1000 tokens).
    pub output_tokens_credit_multiplier_micro: u64,
    /// Human-readable multiplier display string (e.g. "1x", "3x").
    pub multiplier_display: String,
    /// Per-model token estimation budgets for preflight reserve.
    pub estimation_budgets: EstimationBudgets,
    /// Top-k chunks returned by similarity search per `file_search` call.
    pub max_retrieved_chunks_per_turn: u32,
    /// Full general config captured at snapshot time.
    pub general_config: ModelGeneralConfig,
    /// Tenant preference settings captured at snapshot time.
    pub preference: ModelPreference,
}

/// Per-model token estimation budget parameters (API: `PolicyModelEstimationBudgets`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EstimationBudgets {
    /// Conservative bytes-per-token ratio for text estimation.
    pub bytes_per_token_conservative: u32,
    /// Constant overhead for protocol/framing tokens.
    pub fixed_overhead_tokens: u32,
    /// Percentage safety margin applied to text estimation (e.g. 10 means 10%).
    pub safety_margin_pct: u32,
    /// Tokens per image for vision surcharge.
    pub image_token_budget: u32,
    /// Fixed token overhead when `file_search` tool is included.
    pub tool_surcharge_tokens: u32,
    /// Fixed token overhead when `web_search` is enabled.
    pub web_search_surcharge_tokens: u32,
    /// Minimum generation token budget guaranteed regardless of input estimates.
    pub minimal_generation_floor: u32,
}

impl Default for EstimationBudgets {
    fn default() -> Self {
        Self {
            bytes_per_token_conservative: 4,
            fixed_overhead_tokens: 100,
            safety_margin_pct: 10,
            image_token_budget: 1000,
            tool_surcharge_tokens: 500,
            web_search_surcharge_tokens: 500,
            minimal_generation_floor: 50,
        }
    }
}

/// LLM API inference parameters (API: `PolicyModelApiParams`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelApiParams {
    pub temperature: f64,
    pub top_p: f64,
    pub frequency_penalty: f64,
    pub presence_penalty: f64,
    pub stop: Vec<String>,
}

/// Feature capability flags (API: `PolicyModelFeatures`).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFeatures {
    pub streaming: bool,
    pub function_calling: bool,
    pub structured_output: bool,
    pub fine_tuning: bool,
    pub distillation: bool,
    pub fim_completion: bool,
    pub chat_prefix_completion: bool,
}

/// Supported input modalities (API: `PolicyModelInputType`).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInputType {
    pub text: bool,
    pub image: bool,
    pub audio: bool,
    pub video: bool,
}

/// Tool support flags (API: `PolicyModelToolSupport`).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelToolSupport {
    pub web_search: bool,
    pub file_search: bool,
    pub image_generation: bool,
    pub code_interpreter: bool,
    pub computer_use: bool,
    pub mcp: bool,
}

/// Supported API endpoints (API: `PolicyModelSupportedEndpoints`).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSupportedEndpoints {
    pub chat_completions: bool,
    pub responses: bool,
    pub realtime: bool,
    pub assistants: bool,
    pub batch_api: bool,
    pub fine_tuning: bool,
    pub embeddings: bool,
    pub videos: bool,
    pub image_generation: bool,
    pub image_edit: bool,
    pub audio_speech_generation: bool,
    pub audio_transcription: bool,
    pub audio_translation: bool,
    pub moderations: bool,
    pub completions: bool,
}

/// Token credit multipliers (API: `PolicyModelTokenPolicy`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTokenPolicy {
    pub input_tokens_credit_multiplier: f64,
    pub output_tokens_credit_multiplier: f64,
}

/// Estimated performance characteristics (API: `PolicyModelPerformance`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPerformance {
    pub response_latency_ms: u32,
    pub speed_tokens_per_second: u32,
}

/// General configuration from Settings Service (API: `PolicyModelGeneralConfig`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelGeneralConfig {
    /// CTI type identifier of the config.
    #[serde(rename = "type")]
    pub config_type: String,
    /// Model tier CTI identifier.
    pub tier: String,
    #[serde(with = "time::serde::rfc3339")]
    pub available_from: OffsetDateTime,
    pub max_file_size_mb: u32,
    pub api_params: ModelApiParams,
    pub features: ModelFeatures,
    pub input_type: ModelInputType,
    pub tool_support: ModelToolSupport,
    pub supported_endpoints: ModelSupportedEndpoints,
    pub token_policy: ModelTokenPolicy,
    pub performance: ModelPerformance,
}

/// Per-tenant preference settings (API: `PolicyModelPreference`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPreference {
    pub is_default: bool,
    /// Display order in the UI.
    pub sort_order: i32,
}

/// Model pricing/capability tier.
///
/// Serializes as `"Standard"` / `"Premium"` (`PascalCase`).
/// Accepts lowercase aliases (`"standard"`, `"premium"`) on deserialization
/// for compatibility with CCM and DESIGN maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelTier {
    #[serde(alias = "standard")]
    Standard,
    #[serde(alias = "premium")]
    Premium,
}

/// Per-user credit allocations for a specific policy version.
/// NOT part of the immutable shared `PolicySnapshot` (DESIGN.md §5.2.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserLimits {
    pub user_id: Uuid,
    pub policy_version: u64,
    pub standard: TierLimits,
    pub premium: TierLimits,
}

/// Credit limits for a single tier within a billing period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierLimits {
    pub limit_daily_credits_micro: i64,
    pub limit_monthly_credits_micro: i64,
}

/// Token usage reported by the provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageTokens {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Canonical usage event payload published via the outbox after finalization.
///
/// Single canonical type — both the outbox enqueuer (infra) and the plugin
/// `publish_usage()` method use this same struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub chat_id: Uuid,
    pub turn_id: Uuid,
    pub request_id: Uuid,
    pub effective_model: String,
    pub selected_model: String,
    pub terminal_state: String,
    pub billing_outcome: String,
    pub usage: Option<UsageTokens>,
    pub actual_credits_micro: i64,
    pub settlement_method: String,
    pub policy_version_applied: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Attachment tests ──

    #[test]
    fn status_round_trip_all_variants() {
        for status in [
            AttachmentStatus::Pending,
            AttachmentStatus::Ready,
            AttachmentStatus::Failed,
        ] {
            let s = status.as_str();
            let parsed = AttachmentStatus::parse(s);
            assert_eq!(parsed, Some(status), "round-trip failed for {s}");
        }
    }

    #[test]
    fn status_from_str_unknown_returns_none() {
        assert_eq!(AttachmentStatus::parse("unknown"), None);
        assert_eq!(AttachmentStatus::parse(""), None);
        assert_eq!(AttachmentStatus::parse("READY"), None);
    }

    #[test]
    fn kind_round_trip_all_variants() {
        for kind in [AttachmentKind::Document, AttachmentKind::Image] {
            let s = kind.as_str();
            let parsed = AttachmentKind::parse(s);
            assert_eq!(parsed, Some(kind), "round-trip failed for {s}");
        }
    }

    #[test]
    fn kind_from_str_unknown_returns_none() {
        assert_eq!(AttachmentKind::parse("video"), None);
        assert_eq!(AttachmentKind::parse(""), None);
    }

    #[test]
    fn from_content_type_documents() {
        let doc_types = [
            "application/pdf",
            "text/plain",
            "text/markdown",
            "text/csv",
            "application/json",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        ];
        for ct in doc_types {
            assert_eq!(
                AttachmentKind::from_content_type(ct),
                AttachmentKind::Document,
                "{ct} should be Document"
            );
        }
    }

    #[test]
    fn from_content_type_images() {
        for ct in ["image/png", "image/jpeg", "image/webp"] {
            assert_eq!(
                AttachmentKind::from_content_type(ct),
                AttachmentKind::Image,
                "{ct} should be Image"
            );
        }
    }

    #[test]
    fn from_content_type_unknown_defaults_to_document() {
        assert_eq!(
            AttachmentKind::from_content_type("application/octet-stream"),
            AttachmentKind::Document
        );
        assert_eq!(
            AttachmentKind::from_content_type("video/mp4"),
            AttachmentKind::Document
        );
    }

    // ── KillSwitches::default safety invariant ──

    #[test]
    fn kill_switches_default_all_disabled() {
        let ks = KillSwitches::default();
        assert!(!ks.disable_premium_tier);
        assert!(!ks.force_standard_tier);
        assert!(!ks.disable_web_search);
        assert!(!ks.disable_file_search);
        assert!(!ks.disable_images);
    }

    // ── EstimationBudgets::default spec values ──

    #[test]
    fn estimation_budgets_default_matches_spec() {
        let eb = EstimationBudgets::default();
        assert_eq!(eb.bytes_per_token_conservative, 4);
        assert_eq!(eb.fixed_overhead_tokens, 100);
        assert_eq!(eb.safety_margin_pct, 10);
        assert_eq!(eb.image_token_budget, 1000);
        assert_eq!(eb.tool_surcharge_tokens, 500);
        assert_eq!(eb.web_search_surcharge_tokens, 500);
        assert_eq!(eb.minimal_generation_floor, 50);
    }

    // ── ModelGeneralConfig: serde(rename = "type") contract ──

    fn sample_general_config() -> ModelGeneralConfig {
        ModelGeneralConfig {
            config_type: "model.general.v1".to_owned(),
            tier: "premium".to_owned(),
            available_from: OffsetDateTime::UNIX_EPOCH,
            max_file_size_mb: 25,
            api_params: ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
            },
            features: ModelFeatures {
                streaming: true,
                function_calling: false,
                structured_output: false,
                fine_tuning: false,
                distillation: false,
                fim_completion: false,
                chat_prefix_completion: false,
            },
            input_type: ModelInputType {
                text: true,
                image: false,
                audio: false,
                video: false,
            },
            tool_support: ModelToolSupport {
                web_search: false,
                file_search: false,
                image_generation: false,
                code_interpreter: false,
                computer_use: false,
                mcp: false,
            },
            supported_endpoints: ModelSupportedEndpoints {
                chat_completions: true,
                responses: false,
                realtime: false,
                assistants: false,
                batch_api: false,
                fine_tuning: false,
                embeddings: false,
                videos: false,
                image_generation: false,
                image_edit: false,
                audio_speech_generation: false,
                audio_transcription: false,
                audio_translation: false,
                moderations: false,
                completions: false,
            },
            token_policy: ModelTokenPolicy {
                input_tokens_credit_multiplier: 1.0,
                output_tokens_credit_multiplier: 3.0,
            },
            performance: ModelPerformance {
                response_latency_ms: 500,
                speed_tokens_per_second: 100,
            },
        }
    }

    #[test]
    fn general_config_serializes_type_not_config_type() {
        let config = sample_general_config();
        let json = serde_json::to_value(&config).unwrap();

        assert!(json.get("type").is_some(), "expected JSON key 'type'");
        assert!(
            json.get("config_type").is_none(),
            "config_type must not appear in JSON output"
        );
        assert_eq!(json["type"], "model.general.v1");
    }

    #[test]
    fn general_config_serde_roundtrip_preserves_rename() {
        let original = sample_general_config();
        let json = serde_json::to_value(&original).unwrap();
        let deserialized: ModelGeneralConfig = serde_json::from_value(json).unwrap();

        assert_eq!(deserialized.config_type, original.config_type);
        assert_eq!(deserialized.tier, original.tier);
    }

    // ── ModelTier serde representation ──

    #[test]
    fn model_tier_serializes_as_pascal_case() {
        let json = serde_json::to_value(ModelTier::Premium).unwrap();
        assert_eq!(json, serde_json::json!("Premium"));

        let json = serde_json::to_value(ModelTier::Standard).unwrap();
        assert_eq!(json, serde_json::json!("Standard"));
    }

    #[test]
    fn model_tier_deserializes_lowercase_aliases() {
        let premium: ModelTier = serde_json::from_value(serde_json::json!("premium")).unwrap();
        assert_eq!(premium, ModelTier::Premium);

        let standard: ModelTier = serde_json::from_value(serde_json::json!("standard")).unwrap();
        assert_eq!(standard, ModelTier::Standard);
    }

    #[test]
    fn model_tier_rejects_unknown_casing() {
        let result = serde_json::from_value::<ModelTier>(serde_json::json!("PREMIUM"));
        assert!(result.is_err());
    }

    // ── KillSwitches serde roundtrip ──

    #[test]
    fn kill_switches_serde_roundtrip_with_enabled_switches() {
        let ks = KillSwitches {
            disable_premium_tier: true,
            force_standard_tier: false,
            disable_web_search: true,
            disable_file_search: false,
            disable_images: true,
        };
        let json = serde_json::to_value(&ks).unwrap();
        let deserialized: KillSwitches = serde_json::from_value(json).unwrap();

        assert!(deserialized.disable_premium_tier);
        assert!(!deserialized.force_standard_tier);
        assert!(deserialized.disable_web_search);
        assert!(!deserialized.disable_file_search);
        assert!(deserialized.disable_images);
    }

    #[test]
    fn kill_switches_default_roundtrips_all_false() {
        let ks = KillSwitches::default();
        let json = serde_json::to_value(&ks).unwrap();
        let deserialized: KillSwitches = serde_json::from_value(json).unwrap();

        assert!(!deserialized.disable_premium_tier);
        assert!(!deserialized.force_standard_tier);
        assert!(!deserialized.disable_web_search);
        assert!(!deserialized.disable_file_search);
        assert!(!deserialized.disable_images);
    }
}
