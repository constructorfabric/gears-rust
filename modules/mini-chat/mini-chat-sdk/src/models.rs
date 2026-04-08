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

    // ── KillSwitches::default safety invariant ──
    // All kill switches must default to false; a new field defaulting to true
    // would accidentally disable functionality across all tenants.

    #[test]
    fn kill_switches_default_all_disabled() {
        let ks = KillSwitches::default();
        assert!(!ks.disable_premium_tier);
        assert!(!ks.force_standard_tier);
        assert!(!ks.disable_web_search);
        assert!(!ks.disable_file_search);
        assert!(!ks.disable_images);
        assert!(!ks.disable_code_interpreter);
    }

    // ── EstimationBudgets::default spec values ──
    // These defaults are specified in DESIGN.md §B.5.2 and used as the
    // ConfigMap fallback. Changing them silently would alter token estimation
    // for every deployment that relies on defaults.

    #[test]
    fn estimation_budgets_default_matches_spec() {
        let eb = EstimationBudgets::default();
        assert_eq!(eb.bytes_per_token_conservative, 4);
        assert_eq!(eb.fixed_overhead_tokens, 100);
        assert_eq!(eb.safety_margin_pct, 10);
        assert_eq!(eb.image_token_budget, 1000);
        assert_eq!(eb.tool_surcharge_tokens, 500);
        assert_eq!(eb.web_search_surcharge_tokens, 500);
        assert_eq!(eb.code_interpreter_surcharge_tokens, 1000);
        assert_eq!(eb.minimal_generation_floor, 50);
    }

    // ── ModelGeneralConfig: serde(rename = "type") contract ──
    // The upstream API sends `"type"` not `"config_type"`. If the rename
    // attribute is removed, deserialization from the real API breaks.

    fn sample_catalog_entry() -> ModelCatalogEntry {
        ModelCatalogEntry {
            model_id: "test-model".to_owned(),
            provider_model_id: "test-model-v1".to_owned(),
            display_name: "Test Model".to_owned(),
            description: String::new(),
            version: String::new(),
            provider_id: "default".to_owned(),
            provider_display_name: "Default".to_owned(),
            icon: String::new(),
            tier: ModelTier::Standard,
            enabled: true,
            multimodal_capabilities: vec![],
            context_window: 128_000,
            max_output_tokens: 16_384,
            max_input_tokens: 128_000,
            input_tokens_credit_multiplier_micro: 1_000_000,
            output_tokens_credit_multiplier_micro: 3_000_000,
            multiplier_display: "1x".to_owned(),
            estimation_budgets: EstimationBudgets::default(),
            max_retrieved_chunks_per_turn: 5,
            max_tool_calls: 2,
            general_config: sample_general_config(),
            preference: Some(ModelPreference {
                is_default: false,
                sort_order: 0,
            }),
            system_prompt: String::new(),
            thread_summary_prompt: String::new(),
        }
    }

    fn sample_general_config() -> ModelGeneralConfig {
        ModelGeneralConfig {
            config_type: "model.general.v1".to_owned(),
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
    }

    // ── ModelCatalogEntry: optional fields default when absent ──
    // Fields with `#[serde(default)]` must deserialize to sensible values
    // when omitted from JSON, so partial configs don't fail to load.

    #[test]
    fn optional_fields_absent_in_json_deserialize_to_defaults() {
        let mut json = serde_json::to_value(sample_catalog_entry()).unwrap();
        let obj = json.as_object_mut().unwrap();
        obj.remove("description");
        obj.remove("version");
        obj.remove("icon");
        obj.remove("enabled");
        obj.remove("multimodal_capabilities");
        obj.remove("multiplier_display");
        obj.remove("estimation_budgets");
        obj.remove("system_prompt");
        obj.remove("thread_summary_prompt");
        obj.remove("preference");

        let entry: ModelCatalogEntry = serde_json::from_value(json).unwrap();
        assert!(entry.description.is_empty());
        assert!(entry.version.is_empty());
        assert!(entry.icon.is_empty());
        assert!(!entry.enabled);
        assert!(entry.preference.is_none());
        assert!(entry.multimodal_capabilities.is_empty());
        assert!(entry.multiplier_display.is_empty());
        assert_eq!(
            entry.estimation_budgets.bytes_per_token_conservative,
            EstimationBudgets::default().bytes_per_token_conservative
        );
        assert!(entry.system_prompt.is_empty());
        assert!(entry.thread_summary_prompt.is_empty());
    }

    // ── ModelCatalogEntry: estimation_budgets serde contract ──
    // `estimation_budgets` defaults to `EstimationBudgets::default()` when absent.

    #[test]
    fn estimation_budgets_absent_in_json_deserializes_to_default() {
        let mut json = serde_json::to_value(sample_catalog_entry()).unwrap();
        json.as_object_mut().unwrap().remove("estimation_budgets");

        let entry: ModelCatalogEntry = serde_json::from_value(json).unwrap();
        let expected = EstimationBudgets::default();
        assert_eq!(
            entry.estimation_budgets.bytes_per_token_conservative,
            expected.bytes_per_token_conservative
        );
        assert_eq!(
            entry.estimation_budgets.fixed_overhead_tokens,
            expected.fixed_overhead_tokens
        );
        assert_eq!(
            entry.estimation_budgets.safety_margin_pct,
            expected.safety_margin_pct
        );
        assert_eq!(
            entry.estimation_budgets.image_token_budget,
            expected.image_token_budget
        );
        assert_eq!(
            entry.estimation_budgets.tool_surcharge_tokens,
            expected.tool_surcharge_tokens
        );
        assert_eq!(
            entry.estimation_budgets.web_search_surcharge_tokens,
            expected.web_search_surcharge_tokens
        );
        assert_eq!(
            entry.estimation_budgets.code_interpreter_surcharge_tokens,
            expected.code_interpreter_surcharge_tokens
        );
        assert_eq!(
            entry.estimation_budgets.minimal_generation_floor,
            expected.minimal_generation_floor
        );
    }

    #[test]
    fn system_prompt_absent_in_json_deserializes_to_empty() {
        let mut json = serde_json::to_value(sample_catalog_entry()).unwrap();
        json.as_object_mut().unwrap().remove("system_prompt");

        let entry: ModelCatalogEntry = serde_json::from_value(json).unwrap();
        assert!(
            entry.system_prompt.is_empty(),
            "missing system_prompt must deserialize to empty string"
        );
    }

    #[test]
    fn system_prompt_roundtrips() {
        let mut entry = sample_catalog_entry();
        entry.system_prompt = "You are a helpful assistant.".to_owned();

        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["system_prompt"], "You are a helpful assistant.");

        let deserialized: ModelCatalogEntry = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.system_prompt, "You are a helpful assistant.");
    }

    // ── ModelTier serde representation ──
    // Serializes as PascalCase ("Standard"/"Premium") for the UI/API.
    // Accepts lowercase aliases for CCM/DESIGN compatibility.

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
    // Verifies that enabled switches survive serialization and that
    // the default (all-off) state roundtrips correctly.

    #[test]
    fn kill_switches_serde_roundtrip_with_enabled_switches() {
        let ks = KillSwitches {
            disable_premium_tier: true,
            force_standard_tier: false,
            disable_web_search: true,
            disable_file_search: false,
            disable_images: true,
            disable_code_interpreter: false,
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
        assert!(!deserialized.disable_code_interpreter);
    }
}
