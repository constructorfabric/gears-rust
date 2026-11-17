// Created: 2026-05-07 by Constructor Tech
// Updated: 2026-11-10 by Constructor Tech
//! User-facing default inference parameters for a model.
//!
//! [`DefaultInferenceParametersV1`] mirrors the inference-knob subset of the
//! Open Responses request schema (`gts.cf.llmgw.core.create_response_body.v1~`)
//! so the LLM Gateway has a uniform input contract regardless of the
//! underlying provider. The provider-wire defaults that ride alongside this
//! shape (different naming, mutually-exclusive variants, provider-only knobs)
//! live in `provider_settings`, whose shape the provider type schema of the
//! LLM Gateway plugin defines, and are intentionally kept distinct: field
//! names that look universal (`temperature`, `top_p`,
//! `max_output_tokens`) are duplicated on the two surfaces because they are
//! rarely 1:1 with provider-wire parameters in practice.
//!
//! The override policy itself (`allow_parameter_override`,
//! `allow_extra_params`) is **not** part of this struct — those are flat
//! fields on [`crate::models::ModelInfoV1`] applied uniformly across all
//! provider variants.

use crate::models::{ReasoningEffort, ServiceTier};

// ---------------------------------------------------------------------------
// DefaultInferenceParametersV1
// ---------------------------------------------------------------------------

/// Default inference parameters in the unified, user-facing shape.
///
/// All fields are optional — an absent field signals "no default" and the
/// caller's request value (or the provider-wire default on the per-provider
/// settings struct) wins at send time.
#[derive(
    Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[non_exhaustive]
pub struct DefaultInferenceParametersV1 {
    /// Sampling temperature. No min/max constraints — provider ranges differ.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus sampling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Maximum number of output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Maximum number of tool calls per response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    /// Top log-probabilities to return per token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u8>,
    /// Context-truncation strategy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationStrategy>,
    /// Service tier in the unified two-variant shape (`Auto | Default`).
    /// Provider-specific tiers (e.g. `OpenAI` `flex`/`priority`) are expressed
    /// at request time via the override-extras allowlist and live in
    /// `provider_settings`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
    /// Whether the model may issue multiple tool calls in parallel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// Response text-format configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextFormat>,
    /// Reasoning controls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    /// Tool-choice policy in the unified shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
}

// ---------------------------------------------------------------------------
// TextFormat
// ---------------------------------------------------------------------------

/// Response text format configuration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct TextFormat {
    pub format: TextFormatKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<TextVerbosity>,
}

impl Default for TextFormat {
    fn default() -> Self {
        Self {
            format: TextFormatKind::Text,
            verbosity: None,
        }
    }
}

/// Concrete text-format variant.
///
/// Wire format is the externally-tagged Open Responses shape (each variant
/// emits a `{ "type": ... }` object) and matches
/// `gts.cf.llmgw.core.text_format.v1~`.
#[derive(
    Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TextFormatKind {
    /// Plain text — provider default.
    #[default]
    Text,
    /// JSON-mode (no schema constraint).
    JsonObject,
    /// Schema-constrained JSON output.
    JsonSchema {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        schema: Option<serde_json::Value>,
        strict: bool,
    },
}

/// Verbosity level for the response text.
///
/// Wire format is lowercase (`"low" | "medium" | "high"`) to match
/// `gts.cf.llmgw.core.text_format.v1~`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum TextVerbosity {
    Low,
    Medium,
    High,
}

// ---------------------------------------------------------------------------
// ReasoningConfig
// ---------------------------------------------------------------------------

/// Reasoning controls in the unified shape — matches
/// `gts.cf.llmgw.core.reasoning_config.v1~`.
#[derive(
    Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[non_exhaustive]
pub struct ReasoningConfig {
    /// Reasoning effort level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    /// Reasoning summary mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummary>,
}

/// Reasoning summary mode.
///
/// Wire format is lowercase (`"concise" | "detailed" | "auto"`) to match
/// `gts.cf.llmgw.core.reasoning_config.v1~`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningSummary {
    Concise,
    Detailed,
    Auto,
}

// ---------------------------------------------------------------------------
// ToolChoice
// ---------------------------------------------------------------------------

/// Tool-choice policy in the unified Open Responses shape.
///
/// Wire format mirrors `gts.cf.llmgw.core.create_response_body.v1~`, which
/// accepts either a bare lowercase mode string or a tagged object naming a
/// specific tool to force. The two shapes are encoded with an
/// `#[serde(untagged)]` outer enum.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(untagged)]
pub enum ToolChoice {
    /// Mode-only choice — bare string on the wire.
    Mode(ToolChoiceMode),
    /// Force a specific tool by name — tagged object on the wire.
    Named(NamedToolChoice),
}

/// Bare-string tool-choice mode.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoiceMode {
    /// Provider picks.
    Auto,
    /// Provider must call exactly one tool.
    Required,
    /// No tool calling.
    None,
}

/// Named tool-choice — the tagged-object shape with a `type` discriminator.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NamedToolChoice {
    /// Force a specific tool by name.
    Function { name: String },
}

// ---------------------------------------------------------------------------
// TruncationStrategy
// ---------------------------------------------------------------------------

/// Context-truncation strategy.
///
/// Wire format is lowercase (`"auto" | "disabled"`) to match
/// `gts.cf.llmgw.core.create_response_body.v1~`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum TruncationStrategy {
    Auto,
    Disabled,
}
