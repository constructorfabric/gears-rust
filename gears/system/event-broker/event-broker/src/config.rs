//! Operator-facing configuration for the Event Broker module
//! (`DESIGN.md` §4.1 Deployment Modes, example YAML at `DESIGN.md:2363-2391`).

use serde::Deserialize;

/// Which of the four deployment-mode wiring variants this instance runs as.
/// Drives [`crate::module::EventBrokerModule`]'s per-mode service/route
/// gating (`DESIGN.md:2224`'s Deployment Modes table;
/// `docs/ADR/0007-service-decomposition.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentMode {
    Standalone,
    ClusterIngest,
    ClusterDelivery,
    ClusterDispatcher,
}

/// The `[modules.event_broker]` operator config section.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventBrokerConfig {
    pub mode: DeploymentMode,

    /// GTS short alias or full instance ID of the default storage backend,
    /// resolved via GTS plugin discovery (`DESIGN.md` §"Storage Backend
    /// Plugin System").
    pub default_storage_backend: String,

    #[serde(default)]
    pub batch: BatchConfig,
    #[serde(default)]
    pub polling: PollingConfig,
    #[serde(default)]
    pub subscription: SubscriptionConfig,
    #[serde(default)]
    pub workers: WorkersConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchConfig {
    pub max_size: u32,
    pub max_payload_bytes: u64,
}

impl Default for BatchConfig {
    fn default() -> Self {
        // `DESIGN.md` §2.2 Constraints: 100 events / 1 MiB batch hard limit.
        Self {
            max_size: 100,
            max_payload_bytes: 1_048_576,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PollingConfig {
    pub default_timeout_secs: u32,
    pub max_timeout_secs: u32,
}

impl Default for PollingConfig {
    fn default() -> Self {
        // `DESIGN.md` §2.2 Constraints: 30s long-poll max timeout.
        Self {
            default_timeout_secs: 30,
            max_timeout_secs: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionConfig {
    /// ISO 8601 duration (e.g. `"PT30S"`); parsing lives with the real
    /// config-loading implementation, not this skeleton.
    pub default_session_timeout: String,
    pub min_session_timeout: String,
}

impl Default for SubscriptionConfig {
    fn default() -> Self {
        Self {
            default_session_timeout: "PT30S".to_owned(),
            min_session_timeout: "PT1S".to_owned(),
        }
    }
}

/// Broker-owned background workers. Only `reaper` (expired subscriptions +
/// idempotency-key cleanup) lives here - `DESIGN.md` §3.7 Key Invariants
/// states the storage backend owns all event deletion, so no
/// cleaner/retention worker config exists (see
/// `docs/ADR/0007-service-decomposition.md`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkersConfig {
    pub reaper_interval_secs: u32,
}

impl Default for WorkersConfig {
    fn default() -> Self {
        Self {
            reaper_interval_secs: 60,
        }
    }
}
