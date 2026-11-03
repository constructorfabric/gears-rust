//! Gear configuration (`[llm-gateway]`).

use serde::{Deserialize, Serialize};

/// Root configuration of the LLM Gateway gear.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmGatewayConfig {
    /// Protective operational bounds.
    pub limits: LimitsConfig,
}

/// Protective operational bounds (DESIGN §4.5, `limits.*`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    /// Maximum number of sync create-response requests in flight per
    /// instance. A request over the limit fails with `gateway_unavailable`
    /// before model resolution.
    pub max_inflight_sync: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_inflight_sync: 256,
        }
    }
}
