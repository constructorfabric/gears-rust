//! Models for the provider-plugin interface.
//!
//! A provider plugin (see [`crate::plugin_api::LlmGatewayProviderPluginClientV1`])
//! isolates one LLM provider behind a common trait. These types carry the
//! per-call context the core Gateway hands to a plugin, the integration-level
//! capabilities a plugin reports, and the per-model parameter support a plugin
//! reports. Per-model feature capabilities (vision, function calling,
//! streaming, …) are owned by Model Registry (`ModelCapabilities`) and
//! validated by the core before dispatch — they are deliberately not
//! duplicated here.

use std::collections::BTreeSet;

use model_registry_sdk::ModelInfoV1;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

/// Per-call context the core Gateway passes to every provider-plugin method.
///
/// Carries what translation and transport need without the plugin reaching into
/// Model Registry or `CredStore` itself. The core builds it from the resolved
/// model, the provider API key, and the request being served. It is an
/// in-process value and is deliberately not serializable, so the key cannot
/// leak through serialization.
#[derive(Debug, Clone)]
pub struct ProviderCallCtx {
    /// The resolved model info the core fetched from Model Registry, carried
    /// unchanged as the GTS-typed [`ModelInfoV1`] envelope (default
    /// `serde_json::Value` provider settings). Its `gts_type` is the
    /// authoritative provider routing key and equals the resolving plugin's
    /// declared `provider_type`. The plugin narrows it to its typed view (e.g.
    /// `ModelInfoV1<OpenAiSettingsV1>`) via [`ProviderCallCtx::typed_info`] and
    /// reads `provider_model_id`, the endpoint, generic and provider-specific
    /// settings, and pricing from there.
    pub model_info: ModelInfoV1,

    /// Gateway request correlation id (the response `id`), propagated for
    /// tracing across usage, error, and audit events
    /// (`cpt-cf-llm-gateway-fr-request-traceability-v1`).
    pub request_id: String,

    /// Provider API key the core fetched from `CredStore` by the model's secret
    /// reference (`cpt-cf-llm-gateway-fr-provider-credentials-v1`). The plugin
    /// sets it on the outbound provider request, which goes only through OAGW
    /// to the model's approved HTTPS destination. `None` for models without a
    /// secret reference (authentication handled by OAGW, local provider). The
    /// key is redacted in `Debug`, zeroed on drop, and must never be logged or
    /// persisted.
    pub api_key: Option<SecretString>,
}

impl ProviderCallCtx {
    /// Narrow [`Self::model_info`] to a concrete provider view `Q`, validating
    /// its `gts_type` against `Q`'s GTS type id. Delegates to Model Registry's
    /// `ModelInfoV1::try_into_typed`.
    ///
    /// # Errors
    ///
    /// - [`gts::NarrowError::SchemaId`] when `model_info.gts_type` does not match
    ///   `Q`'s GTS type id (the plugin was handed a payload for another provider).
    /// - [`gts::NarrowError::Deserialize`] when the payload can't be
    ///   deserialized into `Q`.
    pub fn typed_info<Q>(&self) -> Result<ModelInfoV1<Q>, gts::NarrowError>
    where
        Q: gts::GtsSchema,
        for<'de> Q: gts::GtsDeserialize<'de>,
    {
        self.model_info.clone().try_into_typed::<Q>()
    }
}

/// How a provider plugin consumes media referenced by input content parts
/// (`input_image`, `input_file`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaInputMode {
    /// Base64 data and external URLs are forwarded to the provider as-is; URLs
    /// stay URLs (`cpt-cf-llm-gateway-fr-vision-v1`).
    UrlForward,
    /// The provider accepts only bytes: the core fetches external URLs before
    /// dispatch and hands the plugin base64 content. `FileStorage` URLs are
    /// resolved the same way from P2 (`cpt-cf-llm-gateway-fr-filestorage-media-v1`).
    PrefetchRequired,
}

/// Integration-level capabilities a provider plugin reports.
///
/// These describe how the provider *integration* behaves — concerns Model
/// Registry does not track. Per-model feature capabilities live in
/// `ModelInfoV1.capabilities` / `supported_api` and are validated by the core
/// together with these (`cpt-cf-llm-gateway-fr-model-capability-check-v1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderPluginCapabilities {
    /// Whether the provider integration can stream responses at all
    /// (independent of any individual model's `streaming` flag).
    pub streaming_transport: bool,

    /// How the plugin expects media input to be delivered.
    pub media_input: MediaInputMode,

    /// Whether the plugin maps the Gateway web search tool (`cf:web_search`)
    /// to a native provider web search tool (`cpt-cf-llm-gateway-fr-web-search-v1`).
    pub web_search: bool,
}

/// A generic request parameter whose support can differ between models of the
/// same provider (`cpt-cf-llm-gateway-fr-generation-parameters-v1`).
///
/// `tool_choice` values and reasoning options are listed separately, because
/// support is per value (`cpt-cf-llm-gateway-fr-tool-calling-v1`,
/// `cpt-cf-llm-gateway-fr-reasoning-v1`).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GenericParameter {
    Temperature,
    TopP,
    MaxOutputTokens,
    MaxToolCalls,
    Seed,
    PresencePenalty,
    FrequencyPenalty,
    TopLogprobs,
    Truncation,
    ServiceTier,
    TextVerbosity,
    SafetyIdentifier,
    /// Cache-control hint; dropped without error when unsupported.
    PromptCacheKey,
    ParallelToolCalls,
    /// `tool_choice = none`.
    ToolChoiceNone,
    /// `tool_choice = auto`.
    ToolChoiceAuto,
    /// `tool_choice = required`.
    ToolChoiceRequired,
    /// `tool_choice` naming a specific function.
    ToolChoiceFunction,
    /// `reasoning.effort`.
    ReasoningEffort,
    /// `reasoning.summary`.
    ReasoningSummary,
    /// `reasoning.budget_tokens`.
    ReasoningBudgetTokens,
    /// Embeddings `input_type`.
    EmbeddingInputType,
}

/// Output sizes an embedding model can produce for `dimensions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmbeddingDimensions {
    /// The model has one fixed size; `dimensions` must equal it.
    Fixed { size: u32 },
    /// Any size in `min..=max`.
    Range { min: u32, max: u32 },
    /// One of the listed sizes.
    OneOf { sizes: Vec<u32> },
}

impl EmbeddingDimensions {
    /// Whether the model can produce vectors of exactly `size`.
    #[must_use]
    pub fn allows(&self, size: u32) -> bool {
        match self {
            Self::Fixed { size: fixed } => *fixed == size,
            Self::Range { min, max } => (*min..=*max).contains(&size),
            Self::OneOf { sizes } => sizes.contains(&size),
        }
    }
}

/// What the plugin supports for one resolved model. Reported by
/// [`crate::plugin_api::LlmGatewayProviderPluginClientV1::model_support`] and
/// used by the core before dispatch to reject (`unsupported_parameter`,
/// `capability_not_supported`) or drop parameters.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelSupport {
    /// Generic parameters the model accepts.
    pub parameters: BTreeSet<GenericParameter>,
    /// Sizes the model can produce for embeddings `dimensions`; `None` when
    /// the model is not an embedding model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_dimensions: Option<EmbeddingDimensions>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_dimensions_accept_only_the_fixed_size() {
        let dims = EmbeddingDimensions::Fixed { size: 1536 };
        assert!(dims.allows(1536));
        assert!(!dims.allows(256));
    }

    #[test]
    fn range_and_list_dimensions() {
        assert!(EmbeddingDimensions::Range { min: 64, max: 3072 }.allows(256));
        assert!(!EmbeddingDimensions::Range { min: 64, max: 3072 }.allows(4096));
        assert!(
            EmbeddingDimensions::OneOf {
                sizes: vec![256, 512]
            }
            .allows(512)
        );
        assert!(
            !EmbeddingDimensions::OneOf {
                sizes: vec![256, 512]
            }
            .allows(1024)
        );
    }

    #[test]
    fn model_support_roundtrip() {
        let support = ModelSupport {
            parameters: [
                GenericParameter::Temperature,
                GenericParameter::ToolChoiceAuto,
            ]
            .into_iter()
            .collect(),
            embedding_dimensions: None,
        };
        let value = serde_json::to_value(&support).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "parameters": ["temperature", "tool_choice_auto"] })
        );
        let back: ModelSupport = serde_json::from_value(value).unwrap();
        assert_eq!(back, support);
    }
}
