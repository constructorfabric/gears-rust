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
    /// Media input policy (`media.*`).
    pub media: MediaConfig,
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
        if self.timeouts.stream_idle.is_zero() {
            return Err("timeouts.stream_idle must be > 0".to_owned());
        }
        if self.timeouts.stream_idle > self.timeouts.stream_max_duration {
            return Err("timeouts.stream_idle must be <= timeouts.stream_max_duration".to_owned());
        }
        for (name, timeout) in self.timeouts.dependency.named() {
            if timeout.is_zero() {
                return Err(format!("timeouts.dependency.{name} must be > 0"));
            }
        }
        self.validate_usage()?;
        self.validate_media()?;
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
    /// `media.*`, `limits.max_inline_media_bytes` and `limits.max_media_fetches`.
    fn validate_media(&self) -> Result<(), String> {
        let media = &self.media;
        if media.fetch_timeout.is_zero() {
            return Err("media.fetch_timeout must be > 0".to_owned());
        }
        if media.total_timeout.is_zero() {
            return Err("media.total_timeout must be > 0".to_owned());
        }
        if media.total_timeout < media.fetch_timeout {
            return Err("media.total_timeout must be >= media.fetch_timeout".to_owned());
        }
        if self.limits.max_media_fetches == 0 {
            return Err("limits.max_media_fetches must be > 0".to_owned());
        }
        if media.max_bytes == 0 {
            return Err("media.max_bytes must be > 0".to_owned());
        }
        if self.limits.max_inline_media_bytes == 0 {
            return Err("limits.max_inline_media_bytes must be > 0".to_owned());
        }
        for (key, types) in [
            ("media.allowed_image_types", &media.allowed_image_types),
            ("media.allowed_file_types", &media.allowed_file_types),
        ] {
            if types.is_empty() {
                return Err(format!("{key} must not be empty"));
            }
            if let Some(entry) = types.iter().find(|entry| !is_media_type_pattern(entry)) {
                return Err(format!(
                    "{key} entry {entry:?} must be a lowercase `type/subtype` or `type/*`"
                ));
            }
        }
        Ok(())
    }

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

/// A `type` or `subtype` token of a MIME type: non-empty, ASCII letters
/// (either case), digits and `!#$&-^_.+`. Shared by the allow-list
/// validation and the media data URI / `Content-Type` checks; `*`, quotes,
/// commas, backslashes and whitespace are not token characters.
pub(crate) fn is_mime_token(part: &str) -> bool {
    !part.is_empty()
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b))
}

/// A lowercase `type/subtype` or `type/*` MIME pattern: no parameters, no
/// `*/*`, no whitespace.
fn is_media_type_pattern(entry: &str) -> bool {
    let token = |part: &str| is_mime_token(part) && !part.bytes().any(|b| b.is_ascii_uppercase());
    entry
        .split_once('/')
        .is_some_and(|(kind, subtype)| token(kind) && (subtype == "*" || token(subtype)))
}

/// Largest accepted `shutdown.usage_drain_reserve`: leaves at least
/// `GEAR_STOP_TIMEOUT - MAX_USAGE_DRAIN_RESERVE - SHUTDOWN_MARGIN` (7s) of the
/// stop timeout for in-flight requests and settles to finish first.
const MAX_USAGE_DRAIN_RESERVE: Duration = Duration::from_secs(20);

/// Safety margin kept free at the end of the gear stop timeout.
const SHUTDOWN_MARGIN: Duration = Duration::from_secs(3);

/// Time between the drain deadline (streams and calls are cut) and the end of
/// the quiescence wait: the cut requests settle and hand their usage over in
/// it.
const CUT_SETTLE_ALLOWANCE: Duration = Duration::from_secs(2);

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
    /// Longest a stream waits for the next provider event (the time the
    /// consumer is not reading is not counted).
    #[serde(with = "humantime_serde")]
    pub stream_idle: Duration,
    /// Per-dependency call timeouts (`timeouts.dependency.*`).
    pub dependency: DependencyTimeouts,
}

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            provider_sync: Duration::from_secs(600),
            stream_max_duration: Duration::from_mins(30),
            stream_idle: Duration::from_secs(120),
            dependency: DependencyTimeouts::default(),
        }
    }
}

/// Call timeouts of the gateway's dependencies (DESIGN §4.5,
/// `timeouts.dependency.<name>`). On expiry a call fails with that
/// dependency's fail-closed code (§ 4.4); usage delivery and settle retry.
/// `CredStore` has no entry while the gateway fetches no keys itself, and the
/// `AuthN` exchange is bounded by `client_credentials.exchange_timeout`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DependencyTimeouts {
    /// PEP evaluation (`AuthZ` Resolver).
    #[serde(with = "humantime_serde")]
    pub authz_resolver: Duration,
    /// `get_tenant_model` (Model Registry).
    #[serde(with = "humantime_serde")]
    pub model_registry: Duration,
    /// Tool schema read (Type Registry).
    #[serde(with = "humantime_serde")]
    pub types_registry: Duration,
    /// `acquire_lease`, `commit_lease` and `release_lease` (Quota
    /// Enforcement).
    #[serde(with = "humantime_serde")]
    pub quota_enforcement: Duration,
    /// `create_usage_records` (Usage Collector).
    #[serde(with = "humantime_serde")]
    pub usage_collector: Duration,
}

impl DependencyTimeouts {
    /// The configured timeouts with their key names.
    #[must_use]
    pub fn named(&self) -> [(&'static str, Duration); 5] {
        [
            ("authz_resolver", self.authz_resolver),
            ("model_registry", self.model_registry),
            ("types_registry", self.types_registry),
            ("quota_enforcement", self.quota_enforcement),
            ("usage_collector", self.usage_collector),
        ]
    }
}

impl Default for DependencyTimeouts {
    fn default() -> Self {
        let default = Duration::from_secs(5);
        Self {
            authz_resolver: default,
            model_registry: default,
            types_registry: default,
            quota_enforcement: default,
            usage_collector: default,
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

    /// The drain deadline after the stop signal: requests still running are
    /// cut (`gateway_unavailable`) once it passes. It leaves
    /// `CUT_SETTLE_ALLOWANCE` of the [`Self::quiescence_budget`] for the cut
    /// requests to settle before the Usage Reporter stops waiting.
    #[must_use]
    pub fn cut_after(&self) -> Duration {
        self.quiescence_budget()
            .saturating_sub(CUT_SETTLE_ALLOWANCE)
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
    /// Maximum decoded size in bytes of one inline media part (a base64 data
    /// URI or `file_data`). Over the limit: `validation_error` (param
    /// `input`) before quota admission and credential retrieval.
    pub max_inline_media_bytes: usize,
    /// Maximum number of `https` media URLs one request may have fetched
    /// (only a `PrefetchRequired` plugin with a media egress route fetches).
    /// Over the limit: `validation_error` (param `input`) before quota
    /// admission and credential retrieval.
    pub max_media_fetches: usize,
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
            max_inline_media_bytes: 20 * 1024 * 1024,
            max_media_fetches: 16,
        }
    }
}

/// Media input policy (DESIGN §3.2 Media Resolver, §4.5, `media.*`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConfig {
    /// Upper bound of one media fetch.
    #[serde(with = "humantime_serde")]
    pub fetch_timeout: Duration,
    /// Upper bound of the whole media resolution of one request (all fetches
    /// together; must be >= `fetch_timeout`). On expiry the first unresolved
    /// part fails with `media_not_found`.
    #[serde(with = "humantime_serde")]
    pub total_timeout: Duration,
    /// Maximum size in bytes of a fetched media response.
    pub max_bytes: usize,
    /// MIME types accepted for image parts (`type/subtype`, or `type/*`).
    pub allowed_image_types: Vec<String>,
    /// MIME types accepted for file parts (`type/subtype`, or `type/*`).
    pub allowed_file_types: Vec<String>,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            fetch_timeout: Duration::from_secs(10),
            total_timeout: Duration::from_secs(30),
            max_bytes: 20 * 1024 * 1024,
            allowed_image_types: ["image/png", "image/jpeg", "image/gif", "image/webp"]
                .map(str::to_owned)
                .to_vec(),
            allowed_file_types: ["application/pdf", "text/*"].map(str::to_owned).to_vec(),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
