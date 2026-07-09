// Created: 2026-05-07 by Constructor Tech
//! User-facing default inference parameters for a model.
//!
//! [`DefaultInferenceParametersV1`] mirrors the inference-knob subset of the
//! Open Responses request schema (`gts.cf.llmgw.core.create_response_body.v1~`)
//! so the LLM Gateway has a uniform input contract regardless of the
//! underlying provider. The provider-wire defaults that ride alongside this
//! shape (different naming, mutually-exclusive variants, provider-only knobs)
//! live on the per-provider settings payload (one typed struct per provider,
//! versioned independently from the envelope) and are intentionally kept
//! distinct: field names that look universal (`temperature`, `top_p`,
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
    /// at request time via the override-extras allowlist and live on the
    /// per-provider settings as `OpenAiServiceTier`.
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
    /// Whether to store the response for later retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
}

// ---------------------------------------------------------------------------
// TextFormat
// ---------------------------------------------------------------------------

/// Response text format configuration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
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
        description: Option<String>,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_enums_wire_format_is_lowercase() {
        // Pinned wire format — must stay lowercase to match the
        // gateway-side gts.cf.llmgw.core.* schemas. If a future change
        // drops the `#[serde(rename_all = "lowercase")]` annotation the
        // gateway will reject our payloads.
        assert_eq!(
            serde_json::to_string(&TextVerbosity::Medium).unwrap(),
            "\"medium\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningSummary::Concise).unwrap(),
            "\"concise\""
        );
        assert_eq!(
            serde_json::to_string(&TruncationStrategy::Disabled).unwrap(),
            "\"disabled\""
        );

        // Round-trip back from lowercase.
        let v: TextVerbosity = serde_json::from_str("\"high\"").unwrap();
        assert_eq!(v, TextVerbosity::High);
        let r: ReasoningSummary = serde_json::from_str("\"auto\"").unwrap();
        assert_eq!(r, ReasoningSummary::Auto);
        let t: TruncationStrategy = serde_json::from_str("\"auto\"").unwrap();
        assert_eq!(t, TruncationStrategy::Auto);
    }

    #[test]
    fn text_format_default_is_text_no_verbosity() {
        let t = TextFormat::default();
        assert_eq!(t.format, TextFormatKind::Text);
        assert!(t.verbosity.is_none());
    }

    #[test]
    fn text_format_kind_wire_format_is_tagged_snake_case() {
        // Pinned to gts.cf.llmgw.core.text_format.v1~ — the gateway expects
        // an externally-tagged object discriminated by the `type` field.
        assert_eq!(
            serde_json::to_value(TextFormatKind::Text).unwrap(),
            serde_json::json!({ "type": "text" }),
        );
        assert_eq!(
            serde_json::to_value(TextFormatKind::JsonObject).unwrap(),
            serde_json::json!({ "type": "json_object" }),
        );
        assert_eq!(
            serde_json::to_value(TextFormatKind::JsonSchema {
                name: "Person".into(),
                description: Some("A person".into()),
                schema: Some(serde_json::json!({"type": "object"})),
                strict: true,
            })
            .unwrap(),
            serde_json::json!({
                "type": "json_schema",
                "name": "Person",
                "description": "A person",
                "schema": { "type": "object" },
                "strict": true,
            }),
        );

        // Round-trip from the gateway-shaped wire form.
        let parsed: TextFormatKind =
            serde_json::from_value(serde_json::json!({"type": "json_object"})).unwrap();
        assert_eq!(parsed, TextFormatKind::JsonObject);
    }

    #[test]
    fn tool_choice_wire_format_matches_create_response_body() {
        // Pinned to gts.cf.llmgw.core.create_response_body.v1~ — the gateway
        // accepts either a bare lowercase mode string or a tagged
        // function-name object.
        assert_eq!(
            serde_json::to_value(ToolChoice::Mode(ToolChoiceMode::Auto)).unwrap(),
            serde_json::Value::String("auto".into()),
        );
        assert_eq!(
            serde_json::to_value(ToolChoice::Mode(ToolChoiceMode::Required)).unwrap(),
            serde_json::Value::String("required".into()),
        );
        assert_eq!(
            serde_json::to_value(ToolChoice::Mode(ToolChoiceMode::None)).unwrap(),
            serde_json::Value::String("none".into()),
        );
        assert_eq!(
            serde_json::to_value(ToolChoice::Named(NamedToolChoice::Function {
                name: "search".into(),
            }))
            .unwrap(),
            serde_json::json!({ "type": "function", "name": "search" }),
        );

        // Untagged round-trip from each shape the gateway sends back.
        let mode: ToolChoice = serde_json::from_str("\"auto\"").unwrap();
        assert_eq!(mode, ToolChoice::Mode(ToolChoiceMode::Auto));
        let named: ToolChoice =
            serde_json::from_value(serde_json::json!({ "type": "function", "name": "lookup" }))
                .unwrap();
        assert_eq!(
            named,
            ToolChoice::Named(NamedToolChoice::Function {
                name: "lookup".into()
            })
        );
    }
}
