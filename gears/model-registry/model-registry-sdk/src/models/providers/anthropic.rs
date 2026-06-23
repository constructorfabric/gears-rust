// Created: 2026-05-07 by Constructor Tech
//! `Anthropic` provider settings — Messages API (`POST /v1/messages`).
//!
//! Flat composition: routing/auth headers, provider-wire parameter defaults,
//! and the nested [`AnthropicCost`] live directly on
//! [`AnthropicSettingsV1`]. The per-model override policy
//! (`allow_parameter_override`, `allow_extra_params`) is **not** here —
//! those are flat fields on [`crate::models::ModelInfoV1`]. Declared as a
//! GTS schema leaf via [`struct_to_gts_schema`]; its parent envelope is
//! `ModelInfoV1<P>`.
//!
//! Per-request fields (`messages`, `model`, `tools`, `metadata`,
//! `cache_control`, `stream`) are intentionally **not** stored as registry
//! defaults — the gateway builds them per call.
//!
//! Note: `supported_api` and `provider_model_id` live on `ModelInfoV1`
//! (common), not on `AnthropicSettingsV1`.

use gts_macros::struct_to_gts_schema;

use crate::models::{ModelInfoV1, ProviderSettings};

// ---------------------------------------------------------------------------
// Service Tier
// ---------------------------------------------------------------------------

/// `Anthropic` service tier. `StandardOnly` opts out of priority capacity.
///
/// Wire format is `snake_case` (`"auto" | "standard_only"`) to match the
/// Anthropic Messages API.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicServiceTier {
    Auto,
    StandardOnly,
}

// ---------------------------------------------------------------------------
// Output effort
// ---------------------------------------------------------------------------

/// `Anthropic` output-shaping effort level. Used by `output_config.effort`;
/// conceptually adjacent to but separate from `thinking.budget_tokens`.
///
/// Wire format is lowercase (`"low" | "medium" | "high" | "xhigh" | "max"`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicOutputEffort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

// ---------------------------------------------------------------------------
// Thinking display
// ---------------------------------------------------------------------------

/// Controls how thinking content appears in the response.
///
/// `Summarized` — thinking is returned normally (default when unset).
/// `Omitted` — thinking content is redacted but a signature is returned for
/// multi-turn continuity.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicThinkingDisplay {
    Summarized,
    Omitted,
}

// ---------------------------------------------------------------------------
// Thinking config
// ---------------------------------------------------------------------------

/// Extended-thinking configuration. Tagged union with three variants on the
/// wire (`type` discriminator).
///
/// **Note:** `Anthropic` flags `type=enabled` as deprecated for newer
/// models — prefer `Adaptive`, which lets the server adapt the thinking
/// budget per request.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AnthropicThinking {
    /// Explicit thinking budget. `Anthropic` enforces
    /// `1024 ≤ budget_tokens < max_tokens` server-side; the SDK does not
    /// range-check.
    Enabled {
        budget_tokens: u32,
        display: Option<AnthropicThinkingDisplay>,
    },
    /// Extended thinking is off.
    Disabled,
    /// Server adapts the thinking budget; recommended replacement for
    /// `Enabled` on newer models.
    Adaptive {
        display: Option<AnthropicThinkingDisplay>,
    },
}

// ---------------------------------------------------------------------------
// Tool choice
// ---------------------------------------------------------------------------

/// Tool-selection policy. Tagged union with four variants on the wire
/// (`type` discriminator).
///
/// `disable_parallel_tool_use` (when present) defaults to `false` per the
/// Anthropic API — i.e. parallel tool calls are allowed unless explicitly
/// disabled.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AnthropicToolChoice {
    /// Model decides whether to call any tool.
    Auto {
        disable_parallel_tool_use: Option<bool>,
    },
    /// Model must call exactly one tool.
    Any {
        disable_parallel_tool_use: Option<bool>,
    },
    /// Force a specific tool by name.
    Tool {
        name: String,
        disable_parallel_tool_use: Option<bool>,
    },
    /// Tool calls are not allowed. `disable_parallel_tool_use` is
    /// meaningless when no tool will be called, so it is not carried.
    None,
}

// ---------------------------------------------------------------------------
// Output format (structured output)
// ---------------------------------------------------------------------------

/// Structured-output format spec for `output_config.format`. Tagged on the
/// wire as `{type: "json_schema", schema: <JSON Schema>}`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicJsonOutputFormat {
    JsonSchema { schema: serde_json::Value },
}

// ---------------------------------------------------------------------------
// Output config
// ---------------------------------------------------------------------------

/// Output-shaping configuration. `effort` and `format` are independent
/// knobs — set either, both, or neither.
#[derive(
    Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct AnthropicOutputConfig {
    pub effort: Option<AnthropicOutputEffort>,
    pub format: Option<AnthropicJsonOutputFormat>,
}

// ---------------------------------------------------------------------------
// Cost
// ---------------------------------------------------------------------------

/// `Anthropic` pricing in micro-credits (`u64`, scaled ×1,000,000 to avoid
/// floating point).
///
/// Token rates are **per 1K tokens**; built-in-tool rates are **per 1K
/// calls**. Anthropic bills cache writes at separate 5-minute and 1-hour
/// tiers (matching the values accepted by `cache_control.ttl`) and cache
/// reads at a third rate.
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
pub struct AnthropicCost {
    pub input_per_1k_micro: Option<u64>,
    pub output_per_1k_micro: Option<u64>,
    /// Matches `cache_control.ttl = "5m"`.
    pub cache_creation_5m_per_1k_micro: Option<u64>,
    /// Matches `cache_control.ttl = "1h"`.
    pub cache_creation_1h_per_1k_micro: Option<u64>,
    pub cache_read_per_1k_micro: Option<u64>,
    /// Built-in web-search tool charge per 1,000 invocations.
    pub web_search_per_1k_calls_micro: Option<u64>,
}

// ---------------------------------------------------------------------------
// Aggregate (flat) settings
// ---------------------------------------------------------------------------

/// `Anthropic` provider settings — the typed payload for
/// `ModelInfoV1<AnthropicSettingsV1>`.
///
/// Flat composition: header-level routing/auth, provider-wire parameter
/// defaults, and the nested [`AnthropicCost`].
///
/// # GTS schema
///
/// - **`schema_id`**: `gts.cf.genai.model.info.v1~cf.genai._.anthropic.v1~`
/// - **base**: `ModelInfoV1` (the generic envelope)
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = ModelInfoV1,
    type_id = "gts.cf.genai.model.info.v1~cf.genai._.anthropic.v1~",
    description = "Anthropic provider settings (Messages API)",
    properties = "oagw_alias,anthropic_version,anthropic_beta,temperature,top_p,top_k,max_tokens,stop_sequences,system,inference_geo,service_tier,container,thinking,tool_choice,output_config,cost"
)]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AnthropicSettingsV1 {
    // ── Connection / auth (header-level; not in the request body) ─────
    /// OAGW upstream alias for credentials and base URL routing.
    pub oagw_alias: String,
    /// Required `anthropic-version` HTTP header value (e.g.
    /// `"2023-06-01"`).
    pub anthropic_version: String,
    /// `anthropic-beta` flag headers (extended thinking, 1M context, …).
    pub anthropic_beta: Vec<String>,

    // ── Provider-wire inference defaults ──────────────────────────────
    /// Sampling temperature (`Anthropic` accepts `0.0..=1.0`; SDK does not
    /// range-check).
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    /// **Required** by `Anthropic` on every request. The SDK default of
    /// `0` is treated as "unset" by callers, forcing them to use max
    /// context size.
    pub max_tokens: u32,
    pub stop_sequences: Option<Vec<String>>,
    /// Default system prompt. The wire surface also accepts a sequence of
    /// text blocks; the registry default is the simpler string form, and
    /// the gateway may translate to a block sequence per request when
    /// needed.
    pub system: Option<String>,
    /// Geographic region hint for inference processing (e.g. `"us"`,
    /// `"eu"`); the workspace's `default_inference_geo` is used when
    /// unset.
    pub inference_geo: Option<String>,
    pub service_tier: Option<AnthropicServiceTier>,
    /// Container identifier for reuse across requests (used by the
    /// code-execution / 1M-context tools).
    pub container: Option<String>,
    pub thinking: Option<AnthropicThinking>,
    pub tool_choice: Option<AnthropicToolChoice>,
    pub output_config: Option<AnthropicOutputConfig>,

    // ── Cost (nested) ─────────────────────────────────────────────────
    pub cost: AnthropicCost,
}

impl ProviderSettings for AnthropicSettingsV1 {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_tokens_default_is_zero_sentinel() {
        let s = AnthropicSettingsV1::default();
        assert_eq!(s.max_tokens, 0);
    }

    #[test]
    fn service_tier_two_variants_with_snake_case_wire() {
        // StandardOnly must serialize as "standard_only".
        assert_eq!(
            serde_json::to_string(&AnthropicServiceTier::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&AnthropicServiceTier::StandardOnly).unwrap(),
            "\"standard_only\""
        );
        let back: AnthropicServiceTier = serde_json::from_str("\"standard_only\"").unwrap();
        assert_eq!(back, AnthropicServiceTier::StandardOnly);
    }

    #[test]
    fn output_effort_five_variants_lowercase_wire() {
        let efforts = [
            (AnthropicOutputEffort::Low, "\"low\""),
            (AnthropicOutputEffort::Medium, "\"medium\""),
            (AnthropicOutputEffort::High, "\"high\""),
            (AnthropicOutputEffort::XHigh, "\"xhigh\""),
            (AnthropicOutputEffort::Max, "\"max\""),
        ];
        for (variant, expected) in efforts {
            assert_eq!(serde_json::to_string(&variant).unwrap(), expected);
        }
    }

    #[test]
    fn thinking_enabled_serializes_with_type_tag() {
        let t = AnthropicThinking::Enabled {
            budget_tokens: 8192,
            display: Some(AnthropicThinkingDisplay::Summarized),
        };
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["type"], "enabled");
        assert_eq!(v["budget_tokens"], 8192);
        assert_eq!(v["display"], "summarized");
    }

    #[test]
    fn thinking_disabled_serializes_unit() {
        let t = AnthropicThinking::Disabled;
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v, serde_json::json!({"type": "disabled"}));
    }

    #[test]
    fn thinking_adaptive_no_budget_tokens() {
        let t = AnthropicThinking::Adaptive {
            display: Some(AnthropicThinkingDisplay::Omitted),
        };
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["type"], "adaptive");
        assert_eq!(v["display"], "omitted");
        assert!(v.get("budget_tokens").is_none());
    }

    #[test]
    fn tool_choice_variants_serialize_with_type_tag() {
        let auto = AnthropicToolChoice::Auto {
            disable_parallel_tool_use: Some(true),
        };
        let any = AnthropicToolChoice::Any {
            disable_parallel_tool_use: None,
        };
        let tool = AnthropicToolChoice::Tool {
            name: "calculator".into(),
            disable_parallel_tool_use: Some(false),
        };
        let none = AnthropicToolChoice::None;

        let auto_v = serde_json::to_value(&auto).unwrap();
        assert_eq!(auto_v["type"], "auto");
        assert_eq!(auto_v["disable_parallel_tool_use"], true);

        let any_v = serde_json::to_value(&any).unwrap();
        assert_eq!(any_v["type"], "any");

        let tool_v = serde_json::to_value(&tool).unwrap();
        assert_eq!(tool_v["type"], "tool");
        assert_eq!(tool_v["name"], "calculator");

        let none_v = serde_json::to_value(&none).unwrap();
        assert_eq!(none_v, serde_json::json!({"type": "none"}));
    }

    #[test]
    fn output_format_serializes_with_json_schema_tag() {
        let f = AnthropicJsonOutputFormat::JsonSchema {
            schema: serde_json::json!({"type": "object"}),
        };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["type"], "json_schema");
        assert_eq!(v["schema"], serde_json::json!({"type": "object"}));
    }

    #[test]
    fn thinking_display_lowercase_wire() {
        assert_eq!(
            serde_json::to_string(&AnthropicThinkingDisplay::Summarized).unwrap(),
            "\"summarized\""
        );
        assert_eq!(
            serde_json::to_string(&AnthropicThinkingDisplay::Omitted).unwrap(),
            "\"omitted\""
        );
    }
}
