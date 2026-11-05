//! Gear configuration (`[llm-gateway]`).

use std::time::Duration;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use usage_collector_sdk::UsageTypeGtsId;

use crate::module::GEAR_STOP_TIMEOUT;

/// Root configuration of the LLM Gateway gear.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmGatewayConfig {
    /// Protective operational bounds.
    pub limits: LimitsConfig,
    /// Streaming behavior.
    pub stream: StreamConfig,
    /// Request durations (`timeouts.*`).
    pub timeouts: TimeoutsConfig,
    /// AI credit quota (`quota.*`).
    pub quota: QuotaConfig,
    /// Usage delivery to Usage Collector (`usage.*`).
    pub usage: UsageConfig,
    /// Graceful shutdown (`shutdown.*`).
    pub shutdown: ShutdownConfig,
    /// Service identity for settle and usage delivery. Required: the module
    /// refuses to start without it (settle is never skipped).
    #[serde(skip_serializing)]
    pub client_credentials: Option<ClientCredentialsConfig>,
}

impl LlmGatewayConfig {
    /// Check the values the gear cannot run with.
    ///
    /// # Errors
    ///
    /// A message naming the first offending key.
    pub fn validate(&self) -> Result<(), String> {
        if self.quota.metric.trim().is_empty() {
            return Err("quota.metric must not be empty".to_owned());
        }
        if self.quota.units_per_credit < 1 {
            return Err("quota.units_per_credit must be >= 1".to_owned());
        }
        if self.quota.settle_retry_backoff < Duration::from_millis(1) {
            return Err("quota.settle_retry_backoff must be >= 1ms".to_owned());
        }
        if self.timeouts.provider_sync.is_zero() {
            return Err("timeouts.provider_sync must be > 0".to_owned());
        }
        if self.timeouts.stream_max_duration.is_zero() {
            return Err("timeouts.stream_max_duration must be > 0".to_owned());
        }
        self.validate_usage()?;
        if let Some(credentials) = &self.client_credentials {
            if credentials.client_id.trim().is_empty() {
                return Err("client_credentials.client_id must not be empty".to_owned());
            }
            if credentials.refresh_interval.is_zero() {
                return Err("client_credentials.refresh_interval must be > 0".to_owned());
            }
            if credentials.min_exchange_interval.is_zero() {
                return Err("client_credentials.min_exchange_interval must be > 0".to_owned());
            }
            if credentials.exchange_timeout.is_zero() {
                return Err("client_credentials.exchange_timeout must be > 0".to_owned());
            }
        }
        Ok(())
    }
}

impl LlmGatewayConfig {
    /// `usage.*` and `shutdown.usage_drain_reserve`.
    fn validate_usage(&self) -> Result<(), String> {
        let usage = &self.usage;
        UsageTypeGtsId::new(usage.usage_type.clone())
            .map_err(|err| format!("usage.usage_type is not a valid usage type: {err}"))?;
        if usage.buffer_capacity == 0 {
            return Err("usage.buffer_capacity must be > 0".to_owned());
        }
        if !(1..=MAX_USAGE_BATCH).contains(&usage.batch_size) {
            return Err(format!(
                "usage.batch_size must be between 1 and {MAX_USAGE_BATCH}"
            ));
        }
        if usage.flush_interval.is_zero() {
            return Err("usage.flush_interval must be > 0".to_owned());
        }
        if usage.retry_backoff.is_zero() {
            return Err("usage.retry_backoff must be > 0".to_owned());
        }
        if self.shutdown.usage_drain_reserve.is_zero() {
            return Err("shutdown.usage_drain_reserve must be > 0".to_owned());
        }
        if self.shutdown.usage_drain_reserve > MAX_USAGE_DRAIN_RESERVE {
            return Err(format!(
                "shutdown.usage_drain_reserve must be <= {}s (the gear stop timeout is {}s and in-flight requests need the rest)",
                MAX_USAGE_DRAIN_RESERVE.as_secs(),
                GEAR_STOP_TIMEOUT.as_secs()
            ));
        }
        Ok(())
    }
}

/// Largest accepted `shutdown.usage_drain_reserve`: leaves at least
/// `GEAR_STOP_TIMEOUT - MAX_USAGE_DRAIN_RESERVE - SHUTDOWN_MARGIN` (7s) of the
/// stop timeout for in-flight requests and settles to finish first.
const MAX_USAGE_DRAIN_RESERVE: Duration = Duration::from_secs(20);

/// Safety margin kept free at the end of the gear stop timeout.
const SHUTDOWN_MARGIN: Duration = Duration::from_secs(3);

/// Usage Collector cap of records per `create_usage_records` call.
const MAX_USAGE_BATCH: usize = 100;

/// Request durations (DESIGN §4.5, `timeouts.*`). Values are humantime
/// strings (`"600s"`, `"30m"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimeoutsConfig {
    /// Maximum duration of a sync call or an embeddings call. Also the base
    /// of the quota lease TTL of those calls.
    #[serde(with = "humantime_serde")]
    pub provider_sync: Duration,
    /// Maximum duration of a stream. Also the base of the quota lease TTL of
    /// streams.
    #[serde(with = "humantime_serde")]
    pub stream_max_duration: Duration,
}

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            provider_sync: Duration::from_secs(600),
            stream_max_duration: Duration::from_mins(30),
        }
    }
}

/// AI credit quota (DESIGN §3.2 Quota Guard, §4.5, `quota.*`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuotaConfig {
    /// GTS instance id of the AI credit metric. Quota admission is always on:
    /// there is no "disabled" mode (DESIGN: admission is never bypassed).
    pub metric: String,
    /// Integer metric units per AI credit (micro-credits).
    pub units_per_credit: u64,
    /// Added to the request's maximum duration to form the lease TTL.
    #[serde(with = "humantime_serde")]
    pub lease_ttl_margin: Duration,
    /// Output allowance of the lease amount when neither the request nor the
    /// model sets an output limit.
    pub fallback_max_output_tokens: u64,
    /// Fixed delay between settle attempts after a transient failure.
    #[serde(with = "humantime_serde")]
    pub settle_retry_backoff: Duration,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            metric: toolkit_gts::gts_id!(
                "cf.core.qe.metric_type.v1~cf.llmgw.gateway.ai_credits.v1"
            )
            .to_owned(),
            units_per_credit: 1_000_000,
            lease_ttl_margin: Duration::from_secs(60),
            fallback_max_output_tokens: 4_096,
            settle_retry_backoff: Duration::from_secs(1),
        }
    }
}

/// Usage delivery (DESIGN §3.2 Usage Reporter, §4.5, `usage.*`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UsageConfig {
    /// GTS id of the usage type of the AI credit usage record. Must derive
    /// from the Usage Collector usage-record base and be declared there with
    /// the metadata keys `model`, `provider_type`, `provider_request_id`,
    /// `complete`, `estimate`, accepting quantity 0.
    pub usage_type: String,
    /// Maximum number of records waiting for delivery. A full buffer drops
    /// the new record (counted and logged); requests are never blocked.
    pub buffer_capacity: usize,
    /// Records per `create_usage_records` call (at most the Usage Collector
    /// batch cap of 100).
    pub batch_size: usize,
    /// Longest wait before a partial batch is sent.
    #[serde(with = "humantime_serde")]
    pub flush_interval: Duration,
    /// Base delay between delivery attempts after a transient failure
    /// (uniform +-20% jitter).
    #[serde(with = "humantime_serde")]
    pub retry_backoff: Duration,
    /// How many times a record is resent after the collector answered
    /// `Internal` (per record or for the whole batch) before it is logged
    /// with its response id and dropped. `0` drops on the first `Internal`.
    /// Unavailability is retried without limit.
    pub max_internal_retries: u32,
}

impl Default for UsageConfig {
    fn default() -> Self {
        Self {
            usage_type: toolkit_gts::gts_id!(
                "cf.core.uc.usage_record.v1~cf.llmgw.gateway.ai_credits.v1"
            )
            .to_owned(),
            buffer_capacity: 10_000,
            batch_size: 100,
            flush_interval: Duration::from_secs(1),
            retry_backoff: Duration::from_secs(1),
            max_internal_retries: 3,
        }
    }
}

/// Graceful shutdown (DESIGN §4.5, `shutdown.*`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShutdownConfig {
    /// Part of the gear stop timeout reserved for draining the usage buffer
    /// after in-flight requests ended; records still undelivered after it
    /// are logged and lost.
    #[serde(with = "humantime_serde")]
    pub usage_drain_reserve: Duration,
}

impl ShutdownConfig {
    /// Longest the Usage Reporter waits after the stop signal for requests in
    /// flight and their settles to finish before it starts the drain:
    /// `stop timeout - usage_drain_reserve - margin`.
    #[must_use]
    pub fn quiescence_budget(&self) -> Duration {
        GEAR_STOP_TIMEOUT
            .saturating_sub(self.usage_drain_reserve)
            .saturating_sub(SHUTDOWN_MARGIN)
    }
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            usage_drain_reserve: Duration::from_secs(5),
        }
    }
}

/// `OAuth2` client credentials of the gear's own service identity.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientCredentialsConfig {
    /// `OAuth2` client identifier.
    pub client_id: String,
    /// `OAuth2` client secret (redacted in `Debug`).
    pub client_secret: SecretString,
    /// Scopes requested from the `IdP`; empty means the `IdP` defaults.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// How long an exchanged service context is reused before the next
    /// exchange (the context carries no expiry of its own).
    #[serde(default = "default_refresh_interval", with = "humantime_serde")]
    pub refresh_interval: Duration,
    /// Minimum time between two exchanges, failed ones included: inside it
    /// callers get the previous failure (or a rate-limit error) instead of
    /// hitting the `IdP` again.
    #[serde(default = "default_min_exchange_interval", with = "humantime_serde")]
    pub min_exchange_interval: Duration,
    /// Upper bound of one exchange call, so a hung `IdP` cannot stall every
    /// settle.
    #[serde(default = "default_exchange_timeout", with = "humantime_serde")]
    pub exchange_timeout: Duration,
}

fn default_refresh_interval() -> Duration {
    Duration::from_secs(300)
}

fn default_min_exchange_interval() -> Duration {
    Duration::from_secs(1)
}

fn default_exchange_timeout() -> Duration {
    Duration::from_secs(10)
}

impl std::fmt::Debug for ClientCredentialsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientCredentialsConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[REDACTED]")
            .field("scopes", &self.scopes)
            .field("refresh_interval", &self.refresh_interval)
            .field("min_exchange_interval", &self.min_exchange_interval)
            .field("exchange_timeout", &self.exchange_timeout)
            .finish()
    }
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
