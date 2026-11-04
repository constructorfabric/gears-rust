//! Gear configuration (`[llm-gateway]`).

use serde::{Deserialize, Serialize};

/// Root configuration of the LLM Gateway gear.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmGatewayConfig {
    /// Protective operational bounds.
    pub limits: LimitsConfig,
    /// Streaming behavior.
    pub stream: StreamConfig,
}

/// Streaming configuration (DESIGN §4.5, `stream.*`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StreamConfig {
    /// Capacity (in events) of the bounded channel between the Stream
    /// Supervisor and the consumer. A full channel applies backpressure to
    /// the plugin stream; it is not an error.
    pub channel_capacity: usize,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 64,
        }
    }
}

/// Protective operational bounds (DESIGN §4.5, `limits.*`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_field_names)] // keys mirror the DESIGN `limits.max_*` names
pub struct LimitsConfig {
    /// Maximum number of sync create-response requests in flight per
    /// instance. A request over the limit fails with `gateway_unavailable`
    /// before model resolution.
    pub max_inflight_sync: usize,
    /// Maximum number of streaming create-response requests in flight per
    /// instance; the slot is held for the lifetime of the stream. A request
    /// over the limit fails with `gateway_unavailable` before model
    /// resolution.
    pub max_inflight_streams: usize,
    /// Maximum number of input items in one request. A request over the
    /// limit fails with `validation_error` before quota admission and
    /// credential retrieval.
    pub max_input_items: usize,
    /// Maximum number of tools in one request (counted as sent, before
    /// resolution). Over the limit: `validation_error`.
    pub max_tools: usize,
    /// Maximum serialized size in bytes of one JSON Schema in a request (a
    /// function tool schema or the structured output schema). Over the limit:
    /// `validation_error`.
    pub max_schema_bytes: usize,
    /// Maximum number of input texts in one embeddings request. Over the
    /// limit: `validation_error` (param `input`) before quota admission and
    /// credential retrieval.
    pub max_embedding_inputs: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_inflight_sync: 256,
            max_inflight_streams: 1_024,
            max_input_items: 1_000,
            max_tools: 128,
            max_schema_bytes: 262_144,
            max_embedding_inputs: 2_048,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
