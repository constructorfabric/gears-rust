//! Per-session LLM configuration primitives consumed by the first-party
//! `LlmGatewayPlugin` (Phase 13).
//!
//! The types declared here are the **public schema surface** of the
//! `gtx.cf.chat_engine.llm_gateway.*` namespace per ADR-0023:
//!
//! - [`LlmPluginConfig`] — the opaque blob mounted under
//!   `PluginCallContext.plugin_config` for the LLM Gateway plugin instance.
//! - [`LlmSummarizationSettings`] — controls how many recent messages are
//!   preserved unsummarized on context overflow.
//! - [`LlmMessageMetadata`] / [`LlmUsage`] / [`FinishReason`] — the trailing
//!   `Complete` event payload carrying model attribution, finish reason, and
//!   token usage.
//!
//! All types `#[derive]` `serde::{Serialize, Deserialize}` so the GTS
//! registry can mint a JSON Schema from them and so the plugin can round-
//! trip values across the wire without bespoke conversions. Validation of
//! incoming JSON (the `plugin_config.config` blob in particular) is
//! performed by [`validate_plugin_config`], which delegates to a structural
//! parse via `serde_json` so the source-chain is preserved per the
//! ADR-0023 rule: "all errors crossing the plugin boundary MUST use the
//! `PluginError::*_with(msg, source)` constructors".
//!
//! Schema IDs live in the [`schema_ids`] module so the plugin registrar
//! (Phase 15) has a single source of truth.
//!
//! Per the Phase 13 debug-redaction rule, none of the structs derive
//! `Debug` automatically when they could carry secrets — but `LlmPluginConfig`
//! deliberately contains only operational settings (no API keys, no auth
//! credentials), so a derived `Debug` is acceptable. The plugin layer is
//! responsible for not embedding these values inside tracing fields.
//
// @cpt-cf-chat-engine-domain-llm-config:p13

use std::time::Duration;

use chat_engine_sdk::error::PluginError;
use modkit_macros::domain_model;
use serde::{Deserialize, Serialize};

/// GTS schema IDs for every type registered by the LLM Gateway plugin.
///
/// These are the load-bearing namespaces from ADR-0023; renaming any of
/// them is a breaking change.
pub mod schema_ids {
    pub const LLM_PLUGIN_CONFIG_SCHEMA_ID: &str =
        "gtx.cf.chat_engine.llm_gateway_plugin_config.v1~";
    pub const LLM_SUMMARIZATION_SETTINGS_SCHEMA_ID: &str =
        "gtx.cf.chat_engine.llm_gateway.summarization_settings.v1~";
    pub const LLM_MESSAGE_METADATA_SCHEMA_ID: &str =
        "gtx.cf.chat_engine.llm_gateway.message_metadata.v1~";
    pub const LLM_USAGE_SCHEMA_ID: &str = "gtx.cf.chat_engine.llm_gateway.usage.v1~";

    pub const LLM_MESSAGE_SCHEMA_ID: &str = "gtx.cf.chat_engine.llm_gateway.message.v1~";
    pub const LLM_MESSAGE_GET_RESPONSE_SCHEMA_ID: &str =
        "gtx.cf.chat_engine.llm_gateway.message_get_response.v1~";
    pub const LLM_MESSAGE_NEW_RESPONSE_SCHEMA_ID: &str =
        "gtx.cf.chat_engine.llm_gateway.message_new_response.v1~";
    pub const LLM_MESSAGE_RECREATE_RESPONSE_SCHEMA_ID: &str =
        "gtx.cf.chat_engine.llm_gateway.message_recreate_response.v1~";
    pub const LLM_STREAMING_COMPLETE_EVENT_SCHEMA_ID: &str =
        "gtx.cf.chat_engine.llm_gateway.streaming_complete_event.v1~";
    pub const LLM_MESSAGE_NEW_EVENT_SCHEMA_ID: &str =
        "gtx.cf.chat_engine.llm_gateway.message_new_event.v1~";
    pub const LLM_SESSION_SUMMARY_EVENT_SCHEMA_ID: &str =
        "gtx.cf.chat_engine.llm_gateway.session_summary_event.v1~";
}

/// Lower bound for `recent_messages_to_keep` per ADR-0023.
pub const RECENT_MESSAGES_TO_KEEP_MIN: u32 = 2;
/// Default `recent_messages_to_keep` per ADR-0023.
pub const RECENT_MESSAGES_TO_KEEP_DEFAULT: u32 = 10;

/// Default `retry_count` for non-streaming upstream calls (Model Registry,
/// summary requests). Streaming forwarding is never retried.
pub const DEFAULT_RETRY_COUNT: u32 = 3;
/// Default `retry_delay_ms` between non-streaming retry attempts.
pub const DEFAULT_RETRY_DELAY_MS: u32 = 1000;
/// Default per-service timeout (ms).
pub const DEFAULT_TIMEOUT_MS: u32 = 30_000;
/// Default circuit-breaker failure threshold.
pub const DEFAULT_CIRCUIT_BREAKER_FAILURE_THRESHOLD: u32 = 5;
/// Default circuit-breaker cooldown (ms).
pub const DEFAULT_CIRCUIT_BREAKER_COOLDOWN_MS: u32 = 60_000;

/// Configuration blob mounted under `PluginCallContext.plugin_config` for
/// the LLM Gateway plugin instance.
///
/// Schema ID: `gtx.cf.chat_engine.llm_gateway_plugin_config.v1~`.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmPluginConfig {
    /// Base URL of the in-process / out-of-process LLM Gateway HTTP
    /// service (e.g., `http://llm-gateway.svc.cluster.local`).
    pub gateway_url: String,

    /// Override of the Model Registry's designated default model. When
    /// absent the plugin uses whatever the registry returns as
    /// `default_model_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,

    /// When present, enables the summarization flow on
    /// `context_overflow:` discriminator. When `None`, the plugin returns
    /// an unsupported error from `on_session_summary` and core propagates
    /// the overflow to the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summarization_settings: Option<LlmSummarizationSettings>,

    /// Maximum retry attempts for **non-streaming** upstream calls
    /// (Model Registry + summary generation). Streaming forwarding is
    /// never retried mid-stream — at-most-once delivery is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,

    /// Initial backoff between retry attempts; exponentially scales per
    /// attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_delay_ms: Option<u32>,
}

impl LlmPluginConfig {
    /// Effective retry count, with the ADR-0023 default of 3 when unset.
    #[must_use]
    pub fn effective_retry_count(&self) -> u32 {
        self.retry_count.unwrap_or(DEFAULT_RETRY_COUNT)
    }

    /// Effective retry delay, with the ADR-0023 default of 1s when unset.
    #[must_use]
    pub fn effective_retry_delay(&self) -> Duration {
        Duration::from_millis(u64::from(
            self.retry_delay_ms.unwrap_or(DEFAULT_RETRY_DELAY_MS),
        ))
    }

    /// True when the plugin supports the summarization flow for this
    /// session-type config.
    #[must_use]
    pub fn summarization_enabled(&self) -> bool {
        self.summarization_settings.is_some()
    }
}

/// Settings controlling the context-overflow summarization split.
///
/// Schema ID: `gtx.cf.chat_engine.llm_gateway.summarization_settings.v1~`.
#[domain_model]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmSummarizationSettings {
    /// Number of most-recent messages preserved unsummarized; MUST be
    /// `>= 2`. Defaults to `10` on deserialization when the field is
    /// omitted.
    #[serde(default = "default_recent_messages_to_keep")]
    pub recent_messages_to_keep: u32,
}

impl LlmSummarizationSettings {
    /// Resolved `recent_messages_to_keep`, clamped to the documented lower
    /// bound `RECENT_MESSAGES_TO_KEEP_MIN`.
    #[must_use]
    pub fn keep_count(&self) -> u32 {
        self.recent_messages_to_keep
            .max(RECENT_MESSAGES_TO_KEEP_MIN)
    }
}

impl Default for LlmSummarizationSettings {
    fn default() -> Self {
        Self {
            recent_messages_to_keep: RECENT_MESSAGES_TO_KEEP_DEFAULT,
        }
    }
}

fn default_recent_messages_to_keep() -> u32 {
    RECENT_MESSAGES_TO_KEEP_DEFAULT
}

/// Finish reason returned by the LLM Gateway. Values match the canonical
/// OpenAI-style enum exposed through ADR-0023.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ContentFilter,
    ToolCalls,
    /// Stream terminated by an upstream error or a `stream_interrupted`
    /// disconnect; the persisted message is partial.
    Error,
}

impl FinishReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ContentFilter => "content_filter",
            Self::ToolCalls => "tool_calls",
            Self::Error => "error",
        }
    }
}

/// Token-usage block carried inside [`LlmMessageMetadata`].
///
/// Schema ID: `gtx.cf.chat_engine.llm_gateway.usage.v1~`.
#[domain_model]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
}

/// Trailing metadata emitted at the end of every assistant message,
/// written as the `metadata` field of a `StreamingCompleteEvent`.
///
/// Schema ID: `gtx.cf.chat_engine.llm_gateway.message_metadata.v1~`.
///
/// `Eq` is intentionally **not** derived because `temperature_used: Option<f32>`
/// is a floating-point value and total equality on floats is unsound.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmMessageMetadata {
    pub model_used: String,
    pub finish_reason: FinishReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature_used: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<LlmUsage>,
}

impl LlmMessageMetadata {
    /// Convenience: serialize to the `Value` shape consumed by
    /// `StreamingCompleteEvent.metadata`.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// Validate a JSON blob against the `LlmPluginConfig` schema using
/// `serde_json` as the structural validator.
///
/// On success returns the parsed [`LlmPluginConfig`]. On failure returns a
/// `PluginError::InvalidInput` whose `source` is the underlying
/// `serde_json::Error`, preserving the cause chain per the Phase 13 rule
/// "must NOT replace a typed error with a stringified one".
///
/// Phase 15 will swap this for a GTS-backed JSON Schema validator that
/// also enforces the documented bounds (`recent_messages_to_keep >= 2`,
/// `gateway_url` non-empty). The Phase 13 implementation already enforces
/// the lower bound via [`LlmSummarizationSettings::keep_count`] and the
/// `gateway_url` non-empty guard below — the two layers are
/// complementary.
///
/// # Errors
///
/// Returns [`PluginError::InvalidInput`] with the serde source attached
/// when the blob does not match the [`LlmPluginConfig`] shape, or when
/// the structural invariants (`gateway_url` non-empty,
/// `recent_messages_to_keep >= 2`) are violated.
pub fn validate_plugin_config(json: &serde_json::Value) -> Result<LlmPluginConfig, PluginError> {
    let parsed: LlmPluginConfig = serde_json::from_value(json.clone())
        .map_err(|e| PluginError::invalid_input_with("invalid LlmPluginConfig blob", e))?;

    // SSRF guard: reject gateway URLs aimed at loopback / link-local /
    // private / metadata-service hosts before the LLM-gateway client ever
    // sees the string. This is the only chokepoint for `gateway_url`;
    // every downstream caller embeds the validated value verbatim.
    crate::infra::url_guard::validate_outbound_url(&parsed.gateway_url, "gateway_url")?;

    if let Some(s) = parsed.summarization_settings {
        if s.recent_messages_to_keep < RECENT_MESSAGES_TO_KEEP_MIN {
            return Err(PluginError::invalid_input(format!(
                "LlmPluginConfig.summarization_settings.recent_messages_to_keep must be >= {RECENT_MESSAGES_TO_KEEP_MIN}",
            )));
        }
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_plugin_config_happy_path() {
        let blob = serde_json::json!({
            "gateway_url": "https://gw.example/v1",
        });
        let cfg = validate_plugin_config(&blob).expect("valid config");
        assert_eq!(cfg.gateway_url, "https://gw.example/v1");
        assert!(cfg.default_model.is_none());
        assert!(!cfg.summarization_enabled());
        assert_eq!(cfg.effective_retry_count(), DEFAULT_RETRY_COUNT);
        assert_eq!(
            cfg.effective_retry_delay(),
            Duration::from_millis(u64::from(DEFAULT_RETRY_DELAY_MS))
        );
    }

    #[test]
    fn validate_plugin_config_with_summarization_defaults() {
        let blob = serde_json::json!({
            "gateway_url": "https://gw.example",
            "summarization_settings": {},
        });
        let cfg = validate_plugin_config(&blob).expect("valid config");
        let s = cfg.summarization_settings.expect("settings present");
        assert_eq!(s.recent_messages_to_keep, RECENT_MESSAGES_TO_KEEP_DEFAULT);
    }

    #[test]
    fn validate_plugin_config_rejects_empty_gateway_url() {
        let blob = serde_json::json!({
            "gateway_url": "  ",
        });
        let err = validate_plugin_config(&blob).expect_err("empty gateway_url");
        assert!(err.to_string().contains("gateway_url"));
    }

    #[test]
    fn validate_plugin_config_rejects_low_recent_messages_to_keep() {
        let blob = serde_json::json!({
            "gateway_url": "https://gw.example",
            "summarization_settings": { "recent_messages_to_keep": 1 },
        });
        let err = validate_plugin_config(&blob).expect_err("violates min");
        assert!(err.to_string().contains("recent_messages_to_keep"));
    }

    #[test]
    fn validate_plugin_config_preserves_source_chain() {
        use std::error::Error;
        // Wrong-shape `gateway_url` (number, not string) — serde fails with
        // a typed error; the plugin error MUST keep it attached.
        let blob = serde_json::json!({ "gateway_url": 42 });
        let err = validate_plugin_config(&blob).expect_err("malformed blob");
        let source = err.source().expect("source preserved");
        assert!(source.to_string().to_ascii_lowercase().contains("string"));
    }

    #[test]
    fn summarization_settings_keep_count_clamps_to_min() {
        // The struct deserializer accepts any u32; `keep_count()` clamps to
        // the lower bound so callers cannot accidentally slice off the
        // entire history.
        let s = LlmSummarizationSettings {
            recent_messages_to_keep: 0,
        };
        assert_eq!(s.keep_count(), RECENT_MESSAGES_TO_KEEP_MIN);
    }

    #[test]
    fn finish_reason_serializes_snake_case() {
        let s = serde_json::to_string(&FinishReason::ContentFilter).unwrap();
        assert_eq!(s, "\"content_filter\"");
    }

    #[test]
    fn llm_message_metadata_round_trips() {
        let meta = LlmMessageMetadata {
            model_used: "gpt-4".into(),
            finish_reason: FinishReason::Stop,
            temperature_used: Some(0.7),
            usage: Some(LlmUsage {
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
                cached_tokens: Some(5),
            }),
        };
        let json = meta.to_json();
        let back: LlmMessageMetadata = serde_json::from_value(json).unwrap();
        assert_eq!(back, meta);
    }

    #[test]
    fn schema_ids_are_stable() {
        // Acts as a tripwire: renaming any of these is a breaking change
        // (clients persist the IDs in the GTS registry).
        assert_eq!(
            schema_ids::LLM_PLUGIN_CONFIG_SCHEMA_ID,
            "gtx.cf.chat_engine.llm_gateway_plugin_config.v1~"
        );
        assert_eq!(
            schema_ids::LLM_SUMMARIZATION_SETTINGS_SCHEMA_ID,
            "gtx.cf.chat_engine.llm_gateway.summarization_settings.v1~"
        );
        assert_eq!(
            schema_ids::LLM_MESSAGE_METADATA_SCHEMA_ID,
            "gtx.cf.chat_engine.llm_gateway.message_metadata.v1~"
        );
        assert_eq!(
            schema_ids::LLM_USAGE_SCHEMA_ID,
            "gtx.cf.chat_engine.llm_gateway.usage.v1~"
        );
    }
}
