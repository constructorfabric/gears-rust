// Created: 2026-07-09 by Constructor Tech
//! Core request/response models — from `schemas/core/`.
//!
//! Covers the Open Responses request body, response resource, embeddings,
//! token usage, and the shared configuration enums referenced from other
//! domains. Size caps present in the schemas (`maxLength`, `minimum`,
//! `maximum`) are documented here but not enforced by types.

use std::collections::HashMap;

use crate::models::{items, tools};

// ---------------------------------------------------------------------------
// CreateResponseBody
// ---------------------------------------------------------------------------

/// Open Responses request body.
///
/// Aside from `model`, every field is optional; an absent field defers to the
/// gateway/provider default.
#[derive(
    Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CreateResponseBody {
    /// Model identifier from the Model Registry.
    pub model: String,
    /// Input items, a single text string, or null.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<ResponseInput>,
    /// System-level instructions for the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Additional data to include in the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<IncludeField>>,
    /// Tools available to the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<tools::Tool>>,
    /// Tool selection strategy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Whether the model may issue multiple tool calls in parallel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// Response text-format configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextFormat>,
    /// Reasoning controls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus sampling parameter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Maximum output tokens (schema minimum 16).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Maximum number of tool calls (schema minimum 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u32>,
    /// Presence penalty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    /// Frequency penalty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    /// Number of top log-probabilities per token (schema range 0..=20).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u8>,
    /// Context-truncation strategy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationStrategy>,
    /// Streaming options.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    /// Service-tier hint for processing priority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
    /// Key-value metadata (schema caps: 16 pairs, 64-char keys, 512-char values).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, String>>,
    /// Safety configuration identifier (schema maximum 64 chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_identifier: Option<String>,
    /// Prompt cache key for reuse across requests (schema maximum 64 chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    /// Provider-/plugin-specific parameters not covered by core fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub extra_fields: Option<HashMap<String, serde_json::Value>>,
}

/// Request `input`: either a single text string or a list of input items.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ResponseInput {
    /// A single text prompt.
    Text(String),
    /// A list of structured input items.
    Items(Vec<items::InputItem>),
}

/// Extra data a caller can request be included in the response.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub enum IncludeField {
    /// Include encrypted reasoning content.
    #[serde(rename = "reasoning.encrypted_content")]
    ReasoningEncryptedContent,
    /// Include output-text log-probabilities.
    #[serde(rename = "message.output_text.logprobs")]
    MessageOutputTextLogprobs,
}

/// Streaming options controlling what the event stream carries.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
pub struct StreamOptions {
    /// Emit a final usage event at the end of the stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_usage: Option<bool>,
}

// ---------------------------------------------------------------------------
// ResponseResource
// ---------------------------------------------------------------------------

/// Open Responses response object.
///
/// Every field is present on the wire; semantically optional fields are
/// nullable and modeled as `Option`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ResponseResource {
    /// Unique response identifier.
    pub id: String,
    /// Object-type discriminator (always `"response"`).
    pub object: String,
    /// Unix timestamp of creation.
    pub created_at: i64,
    /// Unix timestamp of completion, or null.
    pub completed_at: Option<i64>,
    /// Response lifecycle status.
    pub status: ResponseStatus,
    /// Details when `status` is `incomplete`.
    pub incomplete_details: Option<IncompleteDetails>,
    /// Model used for generation.
    pub model: String,
    /// System-level instructions echoed back.
    pub instructions: Option<String>,
    /// Output items produced by the model.
    pub output: Vec<items::OutputItem>,
    /// Error details when `status` is `failed`.
    pub error: Option<ResponseError>,
    /// Tools available for this response.
    pub tools: Vec<tools::Tool>,
    /// Tool selection strategy.
    pub tool_choice: ToolChoice,
    /// Context-truncation strategy.
    pub truncation: TruncationStrategy,
    /// Whether parallel tool calls were enabled.
    pub parallel_tool_calls: bool,
    /// Text-format configuration.
    pub text: TextFormat,
    /// Nucleus sampling parameter.
    pub top_p: f64,
    /// Presence penalty.
    pub presence_penalty: f64,
    /// Frequency penalty.
    pub frequency_penalty: f64,
    /// Number of top log-probabilities returned per token (schema range 0..=20).
    pub top_logprobs: u8,
    /// Sampling temperature.
    pub temperature: f64,
    /// Reasoning configuration.
    pub reasoning: ReasoningConfig,
    /// Token usage; null until the response completes.
    pub usage: Option<Usage>,
    /// Maximum output tokens.
    pub max_output_tokens: Option<u32>,
    /// Maximum number of tool calls.
    pub max_tool_calls: Option<u32>,
    /// Service tier used.
    pub service_tier: String,
    /// Request metadata echoed back.
    pub metadata: Option<HashMap<String, String>>,
    /// Safety configuration identifier.
    pub safety_identifier: Option<String>,
    /// Prompt cache key.
    pub prompt_cache_key: Option<String>,
}

/// Response lifecycle status.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    Queued,
    InProgress,
    Completed,
    Incomplete,
    Failed,
}

/// Reason a response ended incomplete.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct IncompleteDetails {
    /// Machine-readable reason.
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Embeddings
// ---------------------------------------------------------------------------

/// Embedding request.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct EmbeddingRequest {
    /// Model identifier from the Model Registry.
    pub model: String,
    /// Text or array of texts to embed.
    pub input: EmbeddingInput,
    /// Desired output dimensions, if the model supports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
    /// Output encoding format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding_format: Option<EncodingFormat>,
}

/// Embedding `input`: a single text or a batch of texts.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum EmbeddingInput {
    /// A single text.
    Single(String),
    /// A batch of texts.
    Many(Vec<String>),
}

/// Embedding output encoding format.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum EncodingFormat {
    Float,
    Base64,
}

/// Embedding response.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct EmbeddingResponse {
    /// Model used for embedding.
    pub model: String,
    /// Embedding results.
    pub data: Vec<EmbeddingData>,
    /// Token usage.
    pub usage: Usage,
}

/// A single embedding result.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct EmbeddingData {
    /// Index of the input text.
    pub index: u32,
    /// Embedding vector.
    pub embedding: EmbeddingVector,
}

/// Embedding vector: raw floats or a base64-encoded blob.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum EmbeddingVector {
    /// Raw float vector.
    Floats(Vec<f32>),
    /// Base64-encoded vector.
    Base64(String),
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

/// Token usage.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct Usage {
    /// Input tokens consumed.
    pub input_tokens: u64,
    /// Output tokens generated.
    pub output_tokens: u64,
    /// Total tokens (input + output).
    pub total_tokens: u64,
    /// Breakdown of input token usage.
    pub input_tokens_details: InputTokensDetails,
    /// Breakdown of output token usage.
    pub output_tokens_details: OutputTokensDetails,
}

/// Input token usage breakdown.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct InputTokensDetails {
    /// Tokens served from cache.
    pub cached_tokens: u64,
}

/// Output token usage breakdown.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct OutputTokensDetails {
    /// Tokens used for reasoning.
    pub reasoning_tokens: u64,
}

// ---------------------------------------------------------------------------
// ResponseError
// ---------------------------------------------------------------------------

/// Error object within a response resource.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ResponseError {
    /// Machine-readable error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
}

// ---------------------------------------------------------------------------
// TextFormat
// ---------------------------------------------------------------------------

/// Response text-format configuration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TextFormat {
    /// Concrete output format.
    pub format: TextFormatKind,
    /// Text verbosity level.
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

/// Concrete text-format variant — externally tagged by `type`.
#[derive(
    Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TextFormatKind {
    /// Plain text — provider default.
    #[default]
    Text,
    /// JSON mode (no schema constraint).
    JsonObject,
    /// Schema-constrained JSON output.
    JsonSchema {
        /// Schema name.
        name: String,
        /// Schema description.
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// JSON Schema definition.
        #[serde(skip_serializing_if = "Option::is_none")]
        schema: Option<serde_json::Value>,
        /// Strict schema enforcement.
        strict: bool,
    },
}

/// Verbosity level for the response text.
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

/// Reasoning controls.
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ReasoningConfig {
    /// Reasoning effort level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    /// Reasoning summary mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummary>,
}

/// Reasoning effort level.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
    XHigh,
}

/// Reasoning summary mode.
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

/// Tool-choice policy: either a bare mode string or a named-tool object.
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
    /// Provider picks whether and which tool to call.
    Auto,
    /// Provider must call exactly one tool.
    Required,
    /// No tool calling.
    None,
}

/// Named tool-choice — tagged object naming the tool to force.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NamedToolChoice {
    /// Force a specific function tool by name.
    Function {
        /// Function name.
        name: String,
    },
}

// ---------------------------------------------------------------------------
// TruncationStrategy / ServiceTier
// ---------------------------------------------------------------------------

/// Context-truncation strategy.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum TruncationStrategy {
    Auto,
    Disabled,
}

/// Service-tier hint in the request's two-variant shape.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ServiceTier {
    Auto,
    Default,
}

// ---------------------------------------------------------------------------
// Fallback
// ---------------------------------------------------------------------------

/// Provider fallback configuration.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct FallbackConfig {
    /// Ordered list of fallback model identifiers.
    pub models: Vec<String>,
    /// Fallback execution strategy.
    pub strategy: FallbackStrategy,
}

/// Strategy for provider fallback.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum FallbackStrategy {
    Sequential,
    Parallel,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // End-to-end cross-module wiring: a nested request round-trips through the
    // exact wire shape the `schemas/` sources describe.
    #[test]
    fn create_response_body_nested_roundtrip() {
        let wire = serde_json::json!({
            "model": "gpt",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [
                        { "type": "input_text", "text": "describe this" },
                        { "type": "input_image", "image_url": "https://example.test/a.png" }
                    ]
                },
                { "type": "function_call_output", "call_id": "c1", "output": "42" }
            ],
            "tools": [
                { "type": "function", "name": "search" },
                { "type": "cf_gears:image_generation", "quality": "high" }
            ],
            "tool_choice": "auto",
            "text": { "format": { "type": "json_object" } }
        });
        let body: CreateResponseBody = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&body).unwrap(), wire);
    }

    #[test]
    fn response_resource_with_output_roundtrip() {
        let wire = serde_json::json!({
            "id": "resp_1",
            "object": "response",
            "created_at": 1_700_000_000_i64,
            "completed_at": null,
            "status": "completed",
            "incomplete_details": null,
            "model": "gpt",
            "instructions": null,
            "output": [
                {
                    "type": "message",
                    "id": "m1",
                    "status": "completed",
                    "role": "assistant",
                    "content": [
                        { "type": "output_text", "text": "hi", "annotations": [] }
                    ]
                }
            ],
            "error": null,
            "tools": [],
            "tool_choice": "auto",
            "truncation": "disabled",
            "parallel_tool_calls": true,
            "text": { "format": { "type": "text" } },
            "top_p": 1.0,
            "presence_penalty": 0.0,
            "frequency_penalty": 0.0,
            "top_logprobs": 0,
            "temperature": 1.0,
            "reasoning": {},
            "usage": null,
            "max_output_tokens": null,
            "max_tool_calls": null,
            "service_tier": "auto",
            "metadata": null,
            "safety_identifier": null,
            "prompt_cache_key": null
        });
        let resource: ResponseResource = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(resource.status, ResponseStatus::Completed);
        assert_eq!(serde_json::to_value(&resource).unwrap(), wire);
    }

    // A response carrying provider-extension output items and tools the core
    // does not own must round-trip verbatim — extensibility must reach the
    // payload families, not just the top-level event.
    #[test]
    fn response_resource_preserves_extension_item_and_tool() {
        let wire = serde_json::json!({
            "id": "resp_2",
            "object": "response",
            "created_at": 1_700_000_000_i64,
            "completed_at": null,
            "status": "completed",
            "incomplete_details": null,
            "model": "gpt",
            "instructions": null,
            "output": [
                { "type": "openai:web_search_call", "id": "ws_1", "status": "completed", "query": "rust" }
            ],
            "error": null,
            "tools": [
                { "type": "openai:web_search", "search_context_size": "high" }
            ],
            "tool_choice": "auto",
            "truncation": "disabled",
            "parallel_tool_calls": true,
            "text": { "format": { "type": "text" } },
            "top_p": 1.0,
            "presence_penalty": 0.0,
            "frequency_penalty": 0.0,
            "top_logprobs": 0,
            "temperature": 1.0,
            "reasoning": {},
            "usage": null,
            "max_output_tokens": null,
            "max_tool_calls": null,
            "service_tier": "auto",
            "metadata": null,
            "safety_identifier": null,
            "prompt_cache_key": null
        });
        let resource: ResponseResource = serde_json::from_value(wire.clone()).unwrap();
        assert!(matches!(resource.output[0], items::OutputItem::Other(_)));
        assert!(matches!(resource.tools[0], tools::Tool::Other(_)));
        assert_eq!(serde_json::to_value(&resource).unwrap(), wire);
    }

    // -----------------------------------------------------------------------
    // Extra fields
    // -----------------------------------------------------------------------

    #[test]
    fn create_response_body_with_extra_fields_roundtrip() {
        let wire = serde_json::json!({
            "model": "gpt",
            "input": "Hello",
            "extra_fields": {
                "provider_specific_param": { "option": "value" },
                "openai:web_search_options": { "search_context_size": "high" }
            }
        });
        let body: CreateResponseBody = serde_json::from_value(wire.clone()).unwrap();

        let extras = body.extra_fields.as_ref().unwrap();
        assert!(extras.contains_key("provider_specific_param"));
        assert!(extras.contains_key("openai:web_search_options"));
        assert_eq!(
            extras.get("openai:web_search_options"),
            Some(&serde_json::json!({ "search_context_size": "high" }))
        );

        assert_eq!(serde_json::to_value(&body).unwrap(), wire);
    }

    #[test]
    fn create_response_body_extra_fields_defaults_to_none() {
        let body = CreateResponseBody {
            model: "gpt".to_owned(),
            ..Default::default()
        };
        assert!(body.extra_fields.is_none());
    }
}
