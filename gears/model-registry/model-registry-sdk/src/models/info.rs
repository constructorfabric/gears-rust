// Created: 2026-05-06 by Constructor Tech
// Updated: 2026-05-07 by Constructor Tech
//! [`ModelInfoV1<P>`] — provider-independent fields, the user-facing
//! [`DefaultInferenceParametersV1`], the flat per-model override fields
//! (`allow_parameter_override`, `allow_extra_params`), and the typed
//! `provider_settings: P` payload.
//!
//! Declared as the GTS base schema via [`gts_type_schema`]; concrete
//! provider-settings types (e.g. `OpenAiSettingsV1`, shipped in
//! [`crate::models::providers`]) declare themselves as GTS leaves with
//! `base = ModelInfoV1`. The set of provider leaves is open-ended.
//!
//! `P` defaults to `serde_json::Value` (which implements `gts::GtsSchema`
//! upstream) so heterogeneous lists carry an opaque JSON blob; consumers
//! route on [`ModelInfoV1::gts_type`] and narrow to a typed view via
//! [`crate::models::ModelV1::try_into_typed`].

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use toolkit_gts::gts_type_schema;

use crate::models::{
    ContextWindow, DefaultInferenceParametersV1, DisabledCapabilities, ModelCapabilities,
    ModelPerformance, SupportedApi,
};

/// Complete model information: provider-independent metadata, capabilities,
/// the context window, performance, the user-facing default inference
/// parameters, the flat per-model override fields, and the
/// provider-specific `provider_settings` payload.
///
/// `P` defaults to `serde_json::Value` (which implements `gts::GtsSchema`
/// upstream) so heterogeneous lists (e.g. `list_tenant_models`) carry an
/// opaque JSON blob; consumers route on [`ModelInfoV1::gts_type`] and narrow
/// to a typed view via [`crate::models::ModelV1::try_into_typed`].
///
/// # GTS schema
///
/// - **`schema_id`**: `gts_id!("cf.genai.model.info.v1~")`
/// - **base**: yes (root envelope; provider-specific leaves chain off it)
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.genai.model.info.v1~"),
    description = "Generic model info envelope: provider-independent metadata + provider_settings JSON payload",
    properties = "gts_type,display_name,family,vendor,managed,architecture,format,supported_api,provider_model_id,capabilities,context_window,default_parameters"
)]
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ModelInfoV1<P: gts::GtsSchema = serde_json::Value> {
    // ── GTS schema identity ───────────────────────────────────────────
    /// Full GTS schema chain identifying this model's settings shape. Mirrors
    /// `ProviderV1.gts_type` and is the canonical key for resolving the
    /// concrete shape of `provider_settings` (which is a raw JSON blob —
    /// `serde_json::Value` — by default).
    ///
    /// **This field is the authoritative source of truth for provider identity
    /// at runtime.** The generic parameter `P` on `ModelInfoV1<P>` /
    /// `ModelV1<P>` is only a compile-time view of the payload. Any code that
    /// dispatches on the provider — routing, storage, serialization — must read
    /// `gts_type`, never infer the provider from `P`.
    pub gts_type: gts::GtsTypeId,

    // ── Display / discovery ────────────────────────────────────────────
    /// Display name shown in UI.
    pub display_name: String,
    pub description: Option<String>,
    /// Model family (e.g. `"gpt-4"`, `"claude"`, `"llama"`).
    pub family: Option<String>,
    /// Model vendor — the organization that produced the model weights
    /// (e.g. `"OpenAI"`, `"Meta"`). Free-form string; independent of which
    /// provider serves the model.
    pub vendor: Option<String>,
    /// Infrastructure field (for local/managed LLMs): whether Gears can
    /// load/unload **this model** (e.g. install/pull/unload weights on a local
    /// runtime such as Ollama or LM Studio). This is a **per-model** flag and
    /// is distinct from the **per-provider** [`crate::models::ProviderV1::managed`] flag, which
    /// records whether Gears can manage the *provider* at all; a model can only
    /// be `managed` when its provider is also managed. Defaults to `false`
    /// (e.g. for API-only models).
    pub managed: bool,
    /// Infrastructure field (for local/managed LLMs): model architecture
    /// classifier (e.g. `"qwen"`, `"llama"`, `"mistral"`, `"gpt"`). Distinct
    /// from the free-form `family`/`vendor` labels, which are descriptive
    /// marketing/origin labels rather than an architecture taxonomy.
    pub architecture: Option<String>,
    /// Infrastructure field (for local/managed LLMs): on-disk model size in
    /// bytes, used for capacity planning of local/managed weights. `None` for
    /// models whose weights are not locally hosted (e.g. API-only).
    pub size_bytes: Option<u64>,
    /// Infrastructure field (for local/managed LLMs): model weight/serving
    /// format (e.g. `"gguf"`, `"mlx"`, `"safetensors"`, `"api-only"`).
    pub format: Option<String>,
    /// Deployment region (e.g. `"us-east-1"`, `"eu-west-1"`).
    pub region: Option<String>,
    /// Infrastructure host (e.g. `"Azure"`, `"AWS Bedrock"`, `"self-hosted"`).
    pub hosted_by: Option<String>,
    /// When the model version was last released by the vendor.
    pub last_release_at: Option<DateTime<Utc>>,
    /// Informational reasoning level label (e.g. `"high"`, `"medium"`).
    /// Display-only — not used for routing or parameter selection.
    pub reasoning_level: Option<String>,
    /// Model version string.
    pub version: Option<String>,
    /// Display order in model picker / lists.
    pub sort_order: Option<i32>,
    /// URL to model icon.
    pub icon: Option<String>,
    /// Human-readable cost multiplier label (e.g. `"1x"`, `"3x"`).
    pub multiplier_display: Option<String>,
    /// Estimated performance characteristics.
    pub performance: ModelPerformance,
    /// Last-resort escape hatch for deployment-specific metadata; typed
    /// fields on `provider_settings` are preferred.
    pub additional_info: HashMap<String, serde_json::Value>,

    // ── Promoted from the old `ApiResolution` ─────────────────────────
    /// Which API kinds this model exposes (completion, embedding).
    /// Promoted to common so consumers can filter on completion vs
    /// embedding without unwrapping the variant.
    pub supported_api: HashSet<SupportedApi>,
    /// Provider's model identifier — used both in `canonical_id`
    /// (`{provider_slug}::{provider_model_id}`) and sent to the provider in
    /// API requests. Promoted to common so the catalog UI / alias logic
    /// doesn't have to reach into `provider_settings`.
    pub provider_model_id: String,

    // ── Capabilities ───────────────────────────────────────────────────
    /// What the model can do.
    pub capabilities: ModelCapabilities,
    /// Capabilities that are administratively disabled.
    pub disabled_capabilities: DisabledCapabilities,
    /// Token limits.
    pub context_window: ContextWindow,

    // ── User-facing defaults & override policy ────────────────────────
    /// User-facing default inference parameters; mirrors the inference-knob
    /// subset of the Open Responses request schema
    /// (`gts.cf.llmgw.core.create_response_body.v1~`). Distinct from any
    /// provider-wire defaults living on `provider_settings`.
    pub default_parameters: DefaultInferenceParametersV1,
    /// Whether callers may override `default_parameters` per-request. Flat
    /// field on the envelope (no wrapper struct).
    pub allow_parameter_override: bool,
    /// Which extra (non-default) parameter names callers may pass alongside
    /// the request. Flat field on the envelope.
    pub allow_extra_params: Vec<String>,

    // ── Provider-specific payload ──────────────────────────────────────
    /// Provider-specific connection routing, provider-wire default
    /// parameters, and token pricing.
    pub provider_settings: P,
}

impl ModelInfoV1<serde_json::Value> {
    /// Narrow a raw-JSON-payload model info to a typed view by validating
    /// `gts_type` against `Q::TYPE_ID` and deserializing `provider_settings`
    /// into `Q`. Common fields are preserved.
    ///
    /// # Errors
    ///
    /// - [`gts::NarrowError::SchemaId`] when `gts_type` doesn't match `Q::TYPE_ID`.
    /// - [`gts::NarrowError::Deserialize`] when the payload can't be deserialized into `Q`.
    pub fn try_into_typed<Q>(self) -> Result<ModelInfoV1<Q>, gts::NarrowError>
    where
        Q: gts::GtsSchema,
        for<'de> Q: gts::GtsDeserialize<'de>,
    {
        let provider_settings =
            gts::try_narrow::<Q>(self.gts_type.as_ref(), self.provider_settings)?;
        Ok(ModelInfoV1 {
            gts_type: self.gts_type,
            display_name: self.display_name,
            description: self.description,
            family: self.family,
            vendor: self.vendor,
            managed: self.managed,
            architecture: self.architecture,
            size_bytes: self.size_bytes,
            format: self.format,
            region: self.region,
            hosted_by: self.hosted_by,
            last_release_at: self.last_release_at,
            reasoning_level: self.reasoning_level,
            version: self.version,
            sort_order: self.sort_order,
            icon: self.icon,
            multiplier_display: self.multiplier_display,
            performance: self.performance,
            additional_info: self.additional_info,
            supported_api: self.supported_api,
            provider_model_id: self.provider_model_id,
            capabilities: self.capabilities,
            disabled_capabilities: self.disabled_capabilities,
            context_window: self.context_window,
            default_parameters: self.default_parameters,
            allow_parameter_override: self.allow_parameter_override,
            allow_extra_params: self.allow_extra_params,
            provider_settings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use gts::GtsSchema;
    use toolkit_gts::gts_id;

    use crate::models::{
        ContextWindow, DefaultInferenceParametersV1, DisabledCapabilities, MediaCapability,
        ModelCapabilities, OpenAiSettingsV1, ReasoningCapability, WebSearchCapability,
    };

    fn raw_info(gts_type: &str, provider_settings: serde_json::Value) -> ModelInfoV1 {
        ModelInfoV1 {
            gts_type: gts::GtsTypeId::new(gts_type),
            display_name: "Sample".into(),
            description: None,
            family: None,
            vendor: None,
            managed: false,
            architecture: None,
            size_bytes: None,
            format: None,
            region: None,
            hosted_by: None,
            last_release_at: None,
            reasoning_level: None,
            version: None,
            sort_order: None,
            icon: None,
            multiplier_display: None,
            performance: ModelPerformance {
                response_latency_ms: None,
                tokens_per_second: None,
            },
            additional_info: HashMap::new(),
            supported_api: HashSet::from([SupportedApi::Completion]),
            provider_model_id: "gpt-4o".into(),
            capabilities: ModelCapabilities {
                vision: MediaCapability::default(),
                reasoning: ReasoningCapability {
                    effort: false,
                    toggle: false,
                    resume: false,
                    budget: false,
                },
                function_calling: false,
                response_schema: false,
                streaming: false,
                file_input: MediaCapability::default(),
                image_generation: MediaCapability::default(),
                audio_input: MediaCapability::default(),
                audio_output: MediaCapability::default(),
                code_interpreter: false,
                web_search: WebSearchCapability {
                    enabled: false,
                    allowed_domains: false,
                    excluded_domains: false,
                },
            },
            disabled_capabilities: DisabledCapabilities::none(),
            context_window: ContextWindow {
                max_input_tokens: 8192,
                max_output_tokens: Some(4096),
                output_vector_size: None,
            },
            default_parameters: DefaultInferenceParametersV1::default(),
            allow_parameter_override: false,
            allow_extra_params: Vec::new(),
            provider_settings,
        }
    }

    /// Full `OpenAI` settings JSON — the generated deserializer requires every
    /// field to be present (missing `Option` fields are not defaulted).
    fn openai_payload() -> serde_json::Value {
        serde_json::json!({
            "oagw_alias": "openai-prod",
            "endpoint_kind": "chat_completions",
            "organization": null,
            "project": null,
            "temperature": 0.7,
            "top_p": null,
            "presence_penalty": null,
            "frequency_penalty": null,
            "top_logprobs": null,
            "service_tier": null,
            "prompt_cache_retention": null,
            "reasoning_effort": null,
            "reasoning_summary": null,
            "verbosity": null,
            "parallel_tool_calls": null,
            "store": null,
            "response_format": null,
            "max_tokens": 4096,
            "max_completion_tokens": null,
            "n": null,
            "stop": null,
            "seed": null,
            "logprobs": null,
            "max_output_tokens": null,
            "max_tool_calls": null,
            "truncation": null,
            "encoding_format": null,
            "dimensions": null,
            "cost": {
                "input_per_1k_micro": null,
                "cached_input_per_1k_micro": null,
                "output_per_1k_micro": null,
                "long_context_input_per_1k_micro": null,
                "long_context_cached_input_per_1k_micro": null,
                "long_context_output_per_1k_micro": null,
                "long_context_threshold_tokens": null,
                "web_search_per_1k_calls_micro": null,
                "file_search_per_1k_calls_micro": null,
            },
        })
    }

    #[test]
    fn try_into_typed_narrows_matching_schema() {
        let info = raw_info(OpenAiSettingsV1::TYPE_ID, openai_payload());
        let typed: ModelInfoV1<OpenAiSettingsV1> =
            info.try_into_typed().expect("openai schema matches");
        assert_eq!(typed.provider_settings.oagw_alias, "openai-prod");
        assert_eq!(typed.provider_model_id, "gpt-4o");
    }

    #[test]
    fn try_into_typed_fails_on_schema_id_mismatch() {
        let info = raw_info(
            gts_id!("cf.genai.model.info.v1~cf.genai._.anthropic.v1~"),
            serde_json::json!({ "oagw_alias": "openai-prod" }),
        );
        let err = info
            .try_into_typed::<OpenAiSettingsV1>()
            .expect_err("schema id mismatch");
        assert!(matches!(err, gts::NarrowError::SchemaId { .. }));
    }
}
