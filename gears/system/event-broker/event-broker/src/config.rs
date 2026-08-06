//! Operator-facing configuration for the Event Broker module
//! (`DESIGN.md` §4.1 Deployment Modes, example YAML at `DESIGN.md:2363-2391`).

use serde::Deserialize;

/// Transport-level safety cap on `infra::dispatcher::proxy_client::proxy`'s
/// raw, not-yet-typed proxied request body - bounds a read that bypasses any
/// typed extractor entirely, so none of axum's own implicit per-extractor
/// body limits apply to it. Set to axum's own default `Bytes`/`Json`/
/// `String`/`Form` extractor limit (2 MiB) so the dispatcher ends up with the
/// same bound every other handler in this codebase already has implicitly,
/// not a new, arbitrary number. Not operator-configurable - a safety
/// backstop, not a business rule - and distinct from
/// [`BatchConfig::max_payload_bytes`], which is a domain-level rule sized
/// specifically for `events:batch`, not a general transport-safety cap (the
/// dispatcher forwards both ingest and delivery routes, not just batch
/// publishes; see that field's own doc comment for the reverse
/// cross-reference).
pub(crate) const MAX_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;

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
    pub streaming: StreamingConfig,
    #[serde(default)]
    pub workers: WorkersConfig,
    #[serde(default)]
    pub registration: RegistrationConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchConfig {
    pub max_size: u32,
    /// Domain-level, operator-configurable ceiling for `events:batch`
    /// (`DESIGN.md` §2.2) - distinct from
    /// [`MAX_REQUEST_BODY_BYTES`](crate::config::MAX_REQUEST_BODY_BYTES),
    /// the dispatcher's fixed transport-level safety cap on the proxied body
    /// it forwards, which this field does not back (the dispatcher forwards
    /// more than just batch publishes).
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

/// Consumption-stream cadence (`GET /v1/events:stream`/`:sse`).
/// `event-broker-consumption-frames`'s own wording is explicit that the
/// heartbeat cadence is "a constant *configurable* idle cadence (default
/// 5s)" - not a fixed constant - since some intermediaries/load balancers
/// need a shorter cadence to keep an idle connection open than the default
/// assumes (matching gRPC's own per-channel-configurable keepalive
/// interval). Wired into `DeliveryServiceImpl::with_heartbeat_interval` when
/// `module.rs` constructs the service (lands with this change's Group 10 -
/// production routing wiring).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamingConfig {
    pub heartbeat_interval_secs: u32,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: 5,
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

/// `DirectoryService` self-registration address in `cluster_ingest`/
/// `cluster_delivery` mode (`eb-dispatcher-routing` design.md D5) - unused
/// in `standalone`/`cluster_dispatcher` mode. Mirrors `grpc-hub`'s
/// `listen_addr`/`advertise_addr` pattern
/// (`gears/system/grpc-hub/src/gear.rs`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RegistrationConfig {
    /// The address this instance's REST listener binds to.
    pub listen_addr: String,
    /// The address other instances (the dispatcher, in cluster mode) reach
    /// this one at, if different from `listen_addr`. Accepted forms:
    /// `host:<u16>` (literal host and port; `:0` means "use the actual
    /// bound port") or `host` alone (the actual bound port is appended).
    /// `None` falls back to `listen_addr` unless that's a wildcard bind
    /// (`0.0.0.0`), which fails `init` in cluster mode rather than
    /// registering an unroutable address.
    #[serde(default)]
    pub advertise_addr: Option<String>,
}

impl Default for RegistrationConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:0".to_owned(),
            advertise_addr: None,
        }
    }
}
