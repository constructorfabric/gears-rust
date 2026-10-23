// Created: 2026-07-09 by Constructor Tech
//! Core request/response models — from `schemas/core/`.
//!
//! Covers the Open Responses request body, response resource, embeddings,
//! token usage, and the shared configuration enums referenced from other
//! domains. Size caps present in the schemas (`maxLength`, `minimum`,
//! `maximum`) are documented here but not enforced by types.

use std::collections::HashMap;

use rust_decimal::Decimal;

use crate::models::{items, tools};

// ---------------------------------------------------------------------------
// CreateResponseBody
// ---------------------------------------------------------------------------

/// Open Responses request body.
///
/// Aside from `model`, every field is optional; an absent field defers to the
/// gateway/provider default.
///
/// The body has no fields for provider-side conversation state
/// (`previous_response_id`, `store`): the Gateway is stateless and consumers
/// send the full context with every request
/// (`cpt-cf-llm-gateway-fr-chat-completion-v1`).
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
    /// Sampling seed for reproducible generation, where the provider supports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
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
    /// Stream the response as server-sent events (`cpt-cf-llm-gateway-fr-streaming-v1`).
    /// The terminal event always carries usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Gateway extension: drop generic parameters the resolved model does not
    /// support instead of rejecting the request with `unsupported_parameter`.
    /// Every dropped parameter is listed in the response `ignored_parameters`
    /// (`cpt-cf-llm-gateway-fr-generation-parameters-v1`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drop_unsupported_params: Option<bool>,
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
    /// A cache-control hint: dropped without an error when the provider has no
    /// equivalent, and listed in `ignored_parameters`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    /// Provider-specific parameters with no generic equivalent. Which of them
    /// may be forwarded is governed by the model's `allow_extra_params` in
    /// Model Registry.
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
    /// Gateway extension: request parameters that were not applied — dropped
    /// under `drop_unsupported_params`, or unsupported cache-control hints.
    /// Dropping is never silent (`cpt-cf-llm-gateway-fr-generation-parameters-v1`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_parameters: Vec<String>,
    /// Gateway extension: provider originals behind the normalized fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_details: Option<ProviderDetails>,
}

/// Provider originals kept beside the normalized response fields
/// (`cpt-cf-llm-gateway-fr-completion-status-v1`,
/// `cpt-cf-llm-gateway-fr-request-traceability-v1`).
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ProviderDetails {
    /// The provider's original stop reason (e.g. `end_turn`, `length`,
    /// `MAX_TOKENS`), from which `status` and `incomplete_details.reason`
    /// were normalized.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Request identifier assigned by the provider, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
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
    /// Machine-readable reason: [`IncompleteDetails::MAX_OUTPUT_TOKENS`] or
    /// [`IncompleteDetails::CONTENT_FILTER`]
    /// (`cpt-cf-llm-gateway-fr-completion-status-v1`).
    pub reason: String,
}

impl IncompleteDetails {
    /// Output truncated by the output token limit.
    pub const MAX_OUTPUT_TOKENS: &'static str = "max_output_tokens";
    /// Output blocked by the provider's content policy.
    pub const CONTENT_FILTER: &'static str = "content_filter";
}

// ---------------------------------------------------------------------------
// Embeddings
// ---------------------------------------------------------------------------

/// Embedding request (`cpt-cf-llm-gateway-fr-embeddings-v1`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct EmbeddingRequest {
    /// Model identifier from the Model Registry.
    pub model: String,
    /// Text or array of texts to embed.
    pub input: EmbeddingInput,
    /// Output vector size. Never dropped: if the model cannot produce exactly
    /// this size, the request is rejected with `unsupported_parameter`, even
    /// when `drop_unsupported_params` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
    /// Whether the inputs are search queries or documents (e.g. Google
    /// `task_type`, Voyage `input_type`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<EmbeddingInputType>,
    /// Output encoding format. Supported for every model: the Gateway converts
    /// the vectors when the provider does not return the requested format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding_format: Option<EncodingFormat>,
    /// Drop unsupported generic parameters (except `dimensions`) instead of
    /// rejecting the request; dropped ones are listed in `ignored_parameters`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drop_unsupported_params: Option<bool>,
    /// Provider-specific parameters with no generic equivalent, gated by the
    /// model's `allow_extra_params` in Model Registry.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub extra_fields: Option<HashMap<String, serde_json::Value>>,
}

/// Kind of text being embedded.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingInputType {
    /// A search query.
    Query,
    /// A document to be retrieved.
    Document,
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
    /// Gateway request identifier, also the idempotency key of the usage
    /// record (`cpt-cf-llm-gateway-fr-request-traceability-v1`).
    pub id: String,
    /// Model used for embedding.
    pub model: String,
    /// Embedding results.
    pub data: Vec<EmbeddingData>,
    /// Usage: AI credit cost, generic token usage, provider raw usage.
    pub usage: Usage,
    /// Parameters dropped under `drop_unsupported_params`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_parameters: Vec<String>,
    /// Provider originals (`provider_request_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_details: Option<ProviderDetails>,
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

/// Usage of one request (`cpt-cf-llm-gateway-fr-usage-tracking-v1`): cost in
/// AI credits, generic token usage, and the provider's raw usage.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
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
    /// Gateway extension: call cost in AI credits, computed by the provider
    /// plugin from the pricing in the model's provider settings. A JSON
    /// string on the wire (`"0.0125"`) so the amount never passes through a
    /// float.
    #[serde(default, with = "rust_decimal::serde::str")]
    #[schemars(with = "String")]
    pub cost: Decimal,
    /// Gateway extension: the provider-specific raw usage structure, as the
    /// provider returned it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_raw: Option<serde_json::Value>,
    /// Gateway extension: `false` when usage could not be determined reliably
    /// (interrupted stream, consumer disconnect).
    #[serde(default = "default_true")]
    pub complete: bool,
    /// Gateway extension: `true` when the amounts are an estimate rather
    /// than provider-reported usage. Only set together with `complete = false`.
    #[serde(default)]
    pub estimated: bool,
}

fn default_true() -> bool {
    true
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

/// Error object within a response resource, carried by the terminal
/// `response.failed` event of a stream.
///
/// `code` is the same normalized code as [`LlmGatewayError::code`]
/// (`cpt-cf-llm-gateway-fr-provider-error-normalization-v1`).
///
/// [`LlmGatewayError::code`]: crate::errors::LlmGatewayError::code
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ResponseError {
    /// Machine-readable error code.
    pub code: String,
    /// Human-readable error message.
    pub message: String,
    /// Gateway extension: offending request parameter, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    /// Gateway extension: the provider's original error code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_code: Option<String>,
    /// Gateway extension: the provider's original error message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_message: Option<String>,
    /// Gateway extension: request identifier assigned by the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
    /// Gateway extension: seconds to wait before retrying (`rate_limited`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
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

/// Reasoning controls (`cpt-cf-llm-gateway-fr-reasoning-v1`).
///
/// Reasoning is disabled with `effort = none`. An option the model does not
/// support (e.g. `budget_tokens` on an effort-only provider) is rejected with
/// `capability_not_supported`.
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
    /// Gateway extension: reasoning token budget, for providers that take a
    /// budget (e.g. Anthropic `thinking.budget_tokens`, Google `thinkingBudget`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
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
    /// Model must call at least one tool.
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
    fn create_response_body_gateway_extensions_roundtrip() {
        let wire = serde_json::json!({
            "model": "gpt",
            "input": "Hello",
            "seed": 7,
            "reasoning": { "effort": "high", "budget_tokens": 2048 },
            "stream": true,
            "drop_unsupported_params": true
        });
        let body: CreateResponseBody = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(body.stream, Some(true));
        assert_eq!(body.drop_unsupported_params, Some(true));
        assert_eq!(body.reasoning.as_ref().unwrap().budget_tokens, Some(2048));
        assert_eq!(serde_json::to_value(&body).unwrap(), wire);
    }

    #[test]
    fn usage_carries_cost_as_string_and_raw_provider_usage() {
        let wire = serde_json::json!({
            "input_tokens": 10,
            "output_tokens": 5,
            "total_tokens": 15,
            "input_tokens_details": { "cached_tokens": 2 },
            "output_tokens_details": { "reasoning_tokens": 1 },
            "cost": "0.0125",
            "provider_raw": { "prompt_tokens": 10, "completion_tokens": 5 },
            "complete": true,
            "estimated": false
        });
        let usage: Usage = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(usage.cost, Decimal::new(125, 4));
        assert_eq!(serde_json::to_value(&usage).unwrap(), wire);
    }

    #[test]
    fn usage_extensions_default_to_complete_zero_cost() {
        let usage: Usage = serde_json::from_value(serde_json::json!({
            "input_tokens": 1,
            "output_tokens": 1,
            "total_tokens": 2,
            "input_tokens_details": { "cached_tokens": 0 },
            "output_tokens_details": { "reasoning_tokens": 0 }
        }))
        .unwrap();
        assert_eq!(usage.cost, Decimal::ZERO);
        assert!(usage.complete);
        assert!(!usage.estimated);
        assert!(usage.provider_raw.is_none());
    }

    #[test]
    fn embedding_request_with_input_type_roundtrip() {
        let wire = serde_json::json!({
            "model": "embed",
            "input": ["a", "b"],
            "dimensions": 256,
            "input_type": "query",
            "encoding_format": "base64",
            "drop_unsupported_params": true
        });
        let req: EmbeddingRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(req.input_type, Some(EmbeddingInputType::Query));
        assert_eq!(serde_json::to_value(&req).unwrap(), wire);
    }

    #[test]
    fn response_resource_provider_details_roundtrip() {
        let details = ProviderDetails {
            stop_reason: Some("end_turn".into()),
            provider_request_id: Some("req_1".into()),
        };
        let value = serde_json::to_value(&details).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "stop_reason": "end_turn", "provider_request_id": "req_1" })
        );
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
