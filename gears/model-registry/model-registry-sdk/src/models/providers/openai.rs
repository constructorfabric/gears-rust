// Created: 2026-05-06 by Constructor Tech
// Updated: 2026-05-07 by Constructor Tech
//! `OpenAI` provider settings — Chat Completions, Responses, and Embeddings
//! APIs.
//!
//! Flat composition: all routing/auth, provider-wire parameter defaults, and
//! the nested [`OpenAiCost`] live directly on [`OpenAiSettingsV1`]. The
//! per-model override policy (`allow_parameter_override`,
//! `allow_extra_params`) is **not** here — those are flat fields on
//! [`crate::models::ModelInfoV1`]. Declared as a GTS schema leaf via
//! [`struct_to_gts_schema`]; its parent envelope is `ModelInfoV1<P>`.
//!
//! Field set is verified against the `OpenAPI` spec for `POST
//! /v1/chat/completions`, `POST /v1/responses`, and `POST /v1/embeddings`.
//! Per-request fields (`input` / `messages`, `tools`, `tool_choice`,
//! `instructions`, `metadata`, `safety_identifier`, `prompt_cache_key`,
//! `stream`, `stream_options`, `background`, `include`, `conversation`,
//! `modalities`, `audio`, `prediction`, `web_search_options`, `logit_bias`,
//! `function_call` / `functions`) are **not** stored as registry defaults —
//! the gateway builds them per call.
//!
//! Note: `supported_api` and `provider_model_id` live on `ModelInfoV1`
//! (common), not on `OpenAiSettingsV1`.

use gts_macros::struct_to_gts_schema;

use crate::models::{
    ModelInfoV1, ProviderSettings, ReasoningSummary, TextVerbosity, TruncationStrategy,
};

// ---------------------------------------------------------------------------
// Endpoint
// ---------------------------------------------------------------------------

/// Which `OpenAI` surface this connection points at.
///
/// Wire format is `snake_case`. The value is stored verbatim in the JSONB
/// provider-settings payload, so this normalisation also keeps `OData`
/// filters and stored values consistent.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiEndpoint {
    /// Legacy `/v1/chat/completions`.
    ChatCompletions,
    /// New `/v1/responses` (Responses API).
    Responses,
    /// `/v1/embeddings`.
    Embeddings,
}

// ---------------------------------------------------------------------------
// Service Tier (provider-wire, five-variant)
// ---------------------------------------------------------------------------

/// `OpenAI`-specific service tier in the provider-wire shape.
///
/// Distinct from the unified two-variant [`crate::models::ServiceTier`] used
/// by `default_parameters` — the unified shape exposes only `Auto | Default`
/// to mirror the Open Responses request schema; the additional `Flex |
/// Scale | Priority` variants are `OpenAI`-only and ride alongside on this
/// struct. `Scale` was added by `OpenAI` alongside their pricing-tier rollout
/// and is distinct from `Priority`.
///
/// Wire format is lowercase to match the `OpenAI` Chat / Responses APIs.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum OpenAiServiceTier {
    Auto,
    Default,
    Flex,
    Scale,
    Priority,
}

// ---------------------------------------------------------------------------
// Reasoning effort (provider-wire, six-variant)
// ---------------------------------------------------------------------------

/// `OpenAI`-specific reasoning effort level for o-series and gpt-5 reasoning
/// models.
///
/// Distinct from the unified five-variant
/// [`crate::models::ReasoningEffort`] used by
/// `default_parameters.reasoning.effort` — the unified enum stays neutral
/// and exposes only levels every provider understands. `Minimal` was added
/// alongside the gpt-5 reasoning models in mid-2025; it sits between `None`
/// and `Low` and indicates "spend a tiny amount of reasoning effort, then
/// answer". Keeping the OpenAI-specific level here means future
/// OpenAI-only additions don't perturb the shared enum.
///
/// Wire format is lowercase to match the `OpenAI` `reasoning.effort`
/// parameter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum OpenAiReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
}

// ---------------------------------------------------------------------------
// Prompt cache retention
// ---------------------------------------------------------------------------

/// `OpenAI` prompt-cache retention policy. `TwentyFourHours` enables extended
/// prompt caching, which keeps cached prefixes alive for up to 24 hours
/// instead of `OpenAI`'s default in-memory window.
///
/// Wire format matches the literals `OpenAI` accepts on the
/// `prompt_cache_retention` parameter. One of the literals can't be
/// expressed via `rename_all`, so wire spellings are pinned per variant.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub enum OpenAiPromptCacheRetention {
    /// Default in-memory window (cleared frequently).
    #[serde(rename = "in_memory")]
    InMemory,
    /// Extended caching — prefixes kept alive for up to 24 hours.
    #[serde(rename = "24h")]
    TwentyFourHours,
}

// ---------------------------------------------------------------------------
// Embedding encoding
// ---------------------------------------------------------------------------

/// Wire format used to return embedding vectors.
///
/// Wire format is lowercase to match the `OpenAI` Embeddings API
/// `encoding_format` parameter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum OpenAiEmbeddingEncoding {
    /// JSON array of floats (default).
    Float,
    /// Base64-encoded little-endian float32 buffer.
    Base64,
}

// ---------------------------------------------------------------------------
// Response format (structured output)
// ---------------------------------------------------------------------------

/// `response_format` shape supported by Chat Completions / Responses.
///
/// Wire format is the externally-tagged `OpenAI` shape — each variant emits
/// a `{ "type": ... }` object discriminated by the variant name.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAiResponseFormat {
    /// Plain text — provider default.
    Text,
    /// `{ "type": "json_object" }` — JSON-mode.
    JsonObject,
    /// `{ "type": "json_schema", "json_schema": {...} }` — schema-bound output.
    JsonSchema { json_schema: serde_json::Value },
}

// ---------------------------------------------------------------------------
// Cost
// ---------------------------------------------------------------------------

/// `OpenAI` pricing in micro-credits (`u64`, scaled ×1,000,000 to avoid
/// floating point).
///
/// Token rates are **per 1K tokens**; built-in-tool rates are **per 1K
/// calls**. Long-context rates apply when the input length exceeds
/// [`OpenAiCost::long_context_threshold_tokens`] (the standard rates apply
/// below the threshold).
#[allow(clippy::struct_field_names)]
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
pub struct OpenAiCost {
    pub input_per_1k_micro: Option<u64>,
    pub cached_input_per_1k_micro: Option<u64>,
    pub output_per_1k_micro: Option<u64>,

    /// Input rate when above [`OpenAiCost::long_context_threshold_tokens`].
    pub long_context_input_per_1k_micro: Option<u64>,
    /// Cached-input rate when above
    /// [`OpenAiCost::long_context_threshold_tokens`].
    pub long_context_cached_input_per_1k_micro: Option<u64>,
    /// Output rate when above [`OpenAiCost::long_context_threshold_tokens`].
    pub long_context_output_per_1k_micro: Option<u64>,
    /// Input-token boundary above which the long-context rates apply.
    pub long_context_threshold_tokens: Option<u32>,

    /// Built-in web-search tool charge per 1,000 invocations.
    pub web_search_per_1k_calls_micro: Option<u64>,
    /// Built-in file-search tool charge per 1,000 invocations.
    pub file_search_per_1k_calls_micro: Option<u64>,
}

// ---------------------------------------------------------------------------
// Aggregate (flat) settings
// ---------------------------------------------------------------------------

/// `OpenAI` provider settings — the typed payload for `ModelInfoV1<OpenAiSettingsV1>`.
///
/// Flat composition: routing/auth, provider-wire parameter defaults, and the
/// nested [`OpenAiCost`].
///
/// # GTS schema
///
/// - **`schema_id`**: `gts.cf.genai.model.info.v1~cf.genai._.openai.v1~`
/// - **base**: `ModelInfoV1` (the generic envelope)
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = ModelInfoV1,
    type_id = "gts.cf.genai.model.info.v1~cf.genai._.openai.v1~",
    description = "OpenAI provider settings (Chat Completions / Responses / Embeddings)",
    properties = "oagw_alias,endpoint_kind,organization,project,temperature,top_p,presence_penalty,frequency_penalty,top_logprobs,service_tier,prompt_cache_retention,reasoning_effort,reasoning_summary,verbosity,parallel_tool_calls,store,response_format,max_tokens,max_completion_tokens,n,stop,seed,logprobs,max_output_tokens,max_tool_calls,truncation,encoding_format,dimensions,cost"
)]
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAiSettingsV1 {
    // ── Connection / auth ─────────────────────────────────────────────
    /// OAGW upstream alias for credentials and base URL routing.
    pub oagw_alias: String,
    pub endpoint_kind: OpenAiEndpoint,
    /// Optional `OpenAI` organization id.
    pub organization: Option<String>,
    /// Optional `OpenAI` project id.
    pub project: Option<String>,

    // ── Cross-endpoint inference defaults ─────────────────────────────
    /// Sampling temperature (`OpenAI` accepts `0.0..=2.0`; SDK does not
    /// range-check).
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    /// Number of top log-probabilities to return per token (`OpenAI`
    /// accepts 0..=20). Pairs with `logprobs` on the Chat API.
    pub top_logprobs: Option<u8>,
    /// Full five-variant `OpenAI` service tier (`Auto | Default | Flex |
    /// Scale | Priority`); distinct from the unified two-variant
    /// `ServiceTier` on `default_parameters`.
    pub service_tier: Option<OpenAiServiceTier>,
    /// `OpenAI` prompt-cache retention policy.
    pub prompt_cache_retention: Option<OpenAiPromptCacheRetention>,
    /// For o-series and gpt-5 reasoning models. Uses the OpenAI-specific
    /// [`OpenAiReasoningEffort`] enum (six variants, including `Minimal`
    /// which is OpenAI-only). Distinct from the unified five-variant
    /// `ReasoningEffort` on `default_parameters.reasoning.effort`, which
    /// stays neutral.
    pub reasoning_effort: Option<OpenAiReasoningEffort>,
    /// Responses-API `reasoning.summary` knob — uses the shared
    /// [`ReasoningSummary`] enum (`Auto | Concise | Detailed`).
    pub reasoning_summary: Option<ReasoningSummary>,
    /// Chat-API top-level `verbosity` and Responses-API `text.verbosity`
    /// map to the same shape; one registry field covers both.
    pub verbosity: Option<TextVerbosity>,
    pub parallel_tool_calls: Option<bool>,
    /// Chat: whether to store the request for distillation/evals.
    /// Responses: whether to store for retrieval.
    pub store: Option<bool>,
    /// Chat: `response_format`. Responses: ships via `text.format`.
    pub response_format: Option<OpenAiResponseFormat>,

    // ── Chat Completions only ─────────────────────────────────────────
    /// Legacy chat-completions cap. **Deprecated by `OpenAI`** in favor of
    /// `max_completion_tokens`; retained for back-compat with older models.
    pub max_tokens: Option<u32>,
    /// Current chat-completions cap (mutually exclusive with `max_tokens`
    /// in practice).
    pub max_completion_tokens: Option<u32>,
    /// Number of completions per request (`OpenAI` accepts 1..=128).
    pub n: Option<u32>,
    /// Stop sequences.
    pub stop: Option<Vec<String>>,
    /// Deterministic sampling seed. **Marked Beta + deprecated by
    /// `OpenAI`** but still accepted on the wire.
    pub seed: Option<u64>,
    /// Whether to return log probabilities of output tokens.
    pub logprobs: Option<bool>,

    // ── Responses only ────────────────────────────────────────────────
    /// Responses-API output cap. `OpenAI` enforces a minimum of 16
    /// server-side; the SDK does not range-check this value.
    pub max_output_tokens: Option<u32>,
    /// Maximum total built-in tool calls per response.
    pub max_tool_calls: Option<u32>,
    /// Reuses the shared [`TruncationStrategy`] from `default_parameters`
    /// (`Auto | Disabled`). `OpenAI`'s default on the wire is `Disabled`.
    pub truncation: Option<TruncationStrategy>,

    // ── Embeddings only ───────────────────────────────────────────────
    /// `Float | Base64`.
    pub encoding_format: Option<OpenAiEmbeddingEncoding>,
    /// Output embedding dimensionality (text-embedding-3 and later only).
    /// Distinct from `ModelInfoV1.context_window.output_vector_size`: this
    /// field is the **request default** sent on the wire;
    /// `output_vector_size` is the model's intrinsic native dimensionality.
    pub dimensions: Option<u32>,

    // ── Cost (nested) ─────────────────────────────────────────────────
    pub cost: OpenAiCost,
}

impl ProviderSettings for OpenAiSettingsV1 {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // The tests below pin every provider-wire enum to the strings OpenAI
    // accepts. The gateway forwards these literals after merging, so any
    // drift would surface as a parameter-validation error from OpenAI in
    // production. Round-trip cases additionally guard against future
    // refactors that drop the serde directive.

    #[test]
    fn openai_service_tier_wire_format_is_lowercase() {
        for (variant, expected) in [
            (OpenAiServiceTier::Auto, "\"auto\""),
            (OpenAiServiceTier::Default, "\"default\""),
            (OpenAiServiceTier::Flex, "\"flex\""),
            (OpenAiServiceTier::Scale, "\"scale\""),
            (OpenAiServiceTier::Priority, "\"priority\""),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, expected, "wire format drift on {variant:?}");
            let back: OpenAiServiceTier = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn openai_reasoning_effort_wire_format_is_lowercase() {
        for (variant, expected) in [
            (OpenAiReasoningEffort::None, "\"none\""),
            (OpenAiReasoningEffort::Minimal, "\"minimal\""),
            (OpenAiReasoningEffort::Low, "\"low\""),
            (OpenAiReasoningEffort::Medium, "\"medium\""),
            (OpenAiReasoningEffort::High, "\"high\""),
            (OpenAiReasoningEffort::XHigh, "\"xhigh\""),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, expected, "wire format drift on {variant:?}");
            let back: OpenAiReasoningEffort = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn openai_prompt_cache_retention_wire_format() {
        // `"24h"` is a numeric literal that can't come from `rename_all`, so
        // it's pinned per variant.
        for (variant, expected) in [
            (OpenAiPromptCacheRetention::InMemory, "\"in_memory\""),
            (OpenAiPromptCacheRetention::TwentyFourHours, "\"24h\""),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, expected, "wire format drift on {variant:?}");
            let back: OpenAiPromptCacheRetention = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn openai_embedding_encoding_wire_format_is_lowercase() {
        for (variant, expected) in [
            (OpenAiEmbeddingEncoding::Float, "\"float\""),
            (OpenAiEmbeddingEncoding::Base64, "\"base64\""),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, expected, "wire format drift on {variant:?}");
            let back: OpenAiEmbeddingEncoding = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn openai_endpoint_wire_format_is_snake_case() {
        for (variant, expected) in [
            (OpenAiEndpoint::ChatCompletions, "\"chat_completions\""),
            (OpenAiEndpoint::Responses, "\"responses\""),
            (OpenAiEndpoint::Embeddings, "\"embeddings\""),
        ] {
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, expected, "wire format drift on {variant:?}");
            let back: OpenAiEndpoint = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn openai_response_format_is_externally_tagged() {
        assert_eq!(
            serde_json::to_value(OpenAiResponseFormat::Text).unwrap(),
            serde_json::json!({ "type": "text" }),
        );
        assert_eq!(
            serde_json::to_value(OpenAiResponseFormat::JsonObject).unwrap(),
            serde_json::json!({ "type": "json_object" }),
        );
        let schema = serde_json::json!({ "name": "Person", "schema": { "type": "object" } });
        assert_eq!(
            serde_json::to_value(OpenAiResponseFormat::JsonSchema {
                json_schema: schema.clone(),
            })
            .unwrap(),
            serde_json::json!({ "type": "json_schema", "json_schema": schema }),
        );

        // Round-trip from wire shape.
        let parsed: OpenAiResponseFormat =
            serde_json::from_value(serde_json::json!({"type": "json_object"})).unwrap();
        assert_eq!(parsed, OpenAiResponseFormat::JsonObject);
    }
}
