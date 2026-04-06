use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

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
    pub disable_code_interpreter: bool,
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
    #[serde(default)]
    pub description: String,
    /// Model version string.
    #[serde(default)]
    pub version: String,
    /// LLM provider CTI identifier.
    pub provider_id: String,
    /// Routing identifier for provider resolution. Maps to a key in
    /// `MiniChatConfig.providers`. Values: `"openai"`, `"azure_openai"`.
    pub provider_display_name: String,
    /// URL to model icon.
    #[serde(default)]
    pub icon: String,
    /// Model tier (standard or premium).
    pub tier: ModelTier,
    #[serde(default)]
    pub enabled: bool,
    /// Multimodal capability flags, e.g. `VISION_INPUT`, `IMAGE_GENERATION`.
    #[serde(default)]
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
    #[serde(default)]
    pub multiplier_display: String,
    /// Per-model token estimation budgets for preflight reserve.
    #[serde(default)]
    pub estimation_budgets: EstimationBudgets,
    /// Top-k chunks returned by similarity search per `file_search` call.
    pub max_retrieved_chunks_per_turn: u32,
    /// Maximum tool calls the provider may make per request.
    #[serde(default = "default_max_tool_calls")]
    pub max_tool_calls: u32,
    /// Full general config captured at snapshot time.
    pub general_config: ModelGeneralConfig,
    /// Tenant preference settings captured at snapshot time.
    pub preference: Option<ModelPreference>,
    /// System prompt sent as `instructions` in every LLM request for this model.
    /// Empty string = no system instructions.
    #[serde(default)]
    pub system_prompt: String,
    /// Prompt template used when generating thread summaries for this model.
    /// Plumbed through the stack for future use by the summary generation job.
    #[serde(default)]
    pub thread_summary_prompt: String,
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
    /// Fixed token overhead when `code_interpreter` is enabled.
    pub code_interpreter_surcharge_tokens: u32,
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
            code_interpreter_surcharge_tokens: 1000,
            minimal_generation_floor: 50,
        }
    }
}

fn default_max_tool_calls() -> u32 {
    2
}

/// LLM API inference parameters (API: `PolicyModelApiParams`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelApiParams {
    pub temperature: f64,
    pub top_p: f64,
    pub frequency_penalty: f64,
    pub presence_penalty: f64,
    pub stop: Vec<String>,
    /// Provider-specific extra body parameters (e.g. vLLM `top_k`,
    /// `chat_template_kwargs`). Providers that support it will place this
    /// value under the `"extra_body"` key in the request payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Whether a user holds an active `CyberChat` license (API: `CheckUserLicenseResponse`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserLicenseStatus {
    /// `true` if the user's status is `active` in the `active_users` table for this tenant.
    /// `false` if the user is not found, or has status `invited`, `deactivated`, or `deleted`.
    pub active: bool,
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
#[allow(clippy::struct_field_names)]
pub struct UsageTokens {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Tokens served from provider cache (`OpenAI`: `cached_tokens`).
    pub cache_read_input_tokens: u64,
    /// Tokens written to provider cache. Reserved for Anthropic.
    pub cache_write_input_tokens: u64,
    pub reasoning_tokens: u64,
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
    pub web_search_calls: u32,
    pub code_interpreter_calls: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
}
#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
