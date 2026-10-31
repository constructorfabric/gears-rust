use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};

use crate::domain::ssrf::SsrfPolicy;

/// Configuration for the OAGW gear.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OagwConfig {
    #[serde(default = "default_proxy_timeout_secs")]
    pub proxy_timeout_secs: u64,
    #[serde(default = "default_max_body_size_bytes")]
    pub max_body_size_bytes: usize,
    /// Allow plaintext HTTP upstreams (and HTTP token endpoints) for the
    /// proxy. Default: `false`.
    ///
    /// **FIPS:** rejected at startup by [`OagwConfig::validate`] under
    /// `--features fips`; cleartext upstream token endpoints would otherwise
    /// be refused by `toolkit-http` on the first proxied request.
    #[serde(default)]
    pub allow_http_upstream: bool,
    /// TTL in seconds for cached OAuth2 access tokens.
    /// Default: 300 (5 minutes). Kept short because there is currently no
    /// cache-invalidation mechanism — a revoked or rotated token remains
    /// cached until TTL expiry. Increase only if IdP rate limits require it.
    #[serde(default = "default_token_cache_ttl_secs")]
    pub token_cache_ttl_secs: u64,
    /// Maximum number of entries in the OAuth2 token cache.
    /// Default: 10 000.
    #[serde(default = "default_token_cache_capacity")]
    pub token_cache_capacity: usize,
    /// Idle timeout in seconds for WebSocket streaming connections.
    /// A connection with no data in either direction for this duration
    /// will be torn down. Must be > 0. Default: 300 (5 minutes).
    #[serde(default = "default_websocket_idle_timeout_secs")]
    pub websocket_idle_timeout_secs: u64,
    /// Timeout in seconds for announcing a gateway-originated WebSocket close.
    /// On idle timeout or shutdown the gateway gives each leg this long to
    /// accept Close 1001 and half-close. Must be > 0. Default: 5.
    #[serde(default = "default_websocket_close_timeout_secs")]
    pub websocket_close_timeout_secs: u64,
    /// Idle timeout in seconds for SSE streaming connections.
    /// A connection with no data received from upstream for this duration
    /// will be closed. Must be > 0. Default: 300 (5 minutes).
    #[serde(default = "default_streaming_idle_timeout_secs")]
    pub streaming_idle_timeout_secs: u64,
    /// TTL in seconds for cached HTTP protocol version (ALPN) negotiation
    /// results per upstream host. Avoids redundant ALPN re-negotiation on
    /// every connection. Set to 0 to disable the cache entirely (all requests
    /// will use ALPN H2H1 negotiation). Default: 3600 (1 hour).
    #[serde(default = "default_protocol_cache_ttl_secs")]
    pub protocol_cache_ttl_secs: u64,
    /// Whether the upstream/route management REST APIs are enabled.
    /// When `true`, all CRUD endpoints are registered. When `false`, only
    /// read-only endpoints (list / get) are available — write operations
    /// (create / update / delete) are omitted. Default: `true`.
    #[serde(default = "default_true")]
    pub management_api_enabled: bool,
    /// SSRF protection policy. Controls which upstream IPs and hostnames are
    /// blocked. See [`SsrfPolicy`] for available fields.
    /// Default: enabled with built-in deny-list, no extra rules.
    #[serde(default)]
    pub ssrf_policy: SsrfPolicy,
    /// Metrics naming configuration.
    #[serde(default)]
    pub metrics: MetricsConfig,
    /// The registry reconcile on a database backend (ADR-0018 *Startup
    /// Provisioning*).
    #[serde(default)]
    pub registry_provisioning: RegistryProvisioningConfig,
    /// The proxy configuration cache on a database backend (ADR-0020).
    #[serde(default)]
    pub proxy_cache: ProxyCacheConfig,
}

/// The per-replica cache of the configuration the proxy reads. Without a
/// database it has no effect: the in-memory store is read directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyCacheConfig {
    /// Whether proxy resolution reads through the cache. With `false` every
    /// lookup reads the database, writes publish nothing to the cluster change
    /// log, and no cluster is needed. Set it alike on every replica of one
    /// database: replicas that cache see a non-caching replica's writes only
    /// after the TTL. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How long an entry lives: the bound on staleness when a change is not
    /// delivered. Humantime (`60s`). From 1 second to 1 hour. Default:
    /// 60 seconds.
    #[serde(
        default = "default_proxy_cache_ttl",
        with = "toolkit_utils::humantime_serde"
    )]
    pub ttl: Duration,
    /// Entries per section (upstream candidates by alias, routes by
    /// upstream); when a section is full the least recently used entry goes. From
    /// 1 to 1 000 000. Default: 10 000.
    #[serde(default = "default_proxy_cache_max_entries")]
    pub max_entries: usize,
}

impl Default for ProxyCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl: default_proxy_cache_ttl(),
            max_entries: default_proxy_cache_max_entries(),
        }
    }
}

fn default_proxy_cache_ttl() -> Duration {
    Duration::from_mins(1)
}

fn default_proxy_cache_max_entries() -> usize {
    10_000
}

/// Bounds of `proxy_cache.ttl`: below a second the cache barely serves a
/// burst; above an hour a lost change stays visible too long.
const PROXY_CACHE_TTL_MIN: Duration = Duration::from_secs(1);
const PROXY_CACHE_TTL_MAX: Duration = Duration::from_hours(1);

/// Upper bound of `proxy_cache.max_entries`, per section: two sections of a
/// million entries can hold gigabytes, so a larger value is a typo.
const PROXY_CACHE_MAX_ENTRIES_MAX: usize = 1_000_000;

/// The registry reconcile on a database backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryProvisioningConfig {
    /// The deployment's generation, monotonic across releases and rollbacks
    /// (Helm: `.Release.Revision`). A pod whose generation is older than the
    /// one stored by another pod does not reconcile. Unset: the guard is off
    /// (a startup warning says so), and an older pod can revert newer
    /// registry content. Ignored on the in-memory backend. At most
    /// `i64::MAX` (stored as BIGINT).
    #[serde(default)]
    pub generation: Option<u64>,
    /// How often every pod re-checks the registry state after a run, whatever
    /// its outcome (an unchanged state costs one query), with ±20% jitter.
    /// Humantime (`5m`). From 1 second to 24 hours. Default: 5 minutes.
    /// Ignored on the in-memory backend, which reconciles once at startup.
    #[serde(
        default = "default_recheck_interval",
        with = "toolkit_utils::humantime_serde"
    )]
    pub recheck_interval: Duration,
}

impl Default for RegistryProvisioningConfig {
    fn default() -> Self {
        Self {
            generation: None,
            recheck_interval: default_recheck_interval(),
        }
    }
}

fn default_recheck_interval() -> Duration {
    Duration::from_mins(5)
}

/// Bounds of `registry_provisioning.recheck_interval`: below a second every
/// pod hammers the registry-state row; above a day a reverted or failed
/// state waits too long (and the jitter's arithmetic stays far from
/// `Duration`'s range).
const RECHECK_INTERVAL_MIN: Duration = Duration::from_secs(1);
const RECHECK_INTERVAL_MAX: Duration = Duration::from_hours(24);

/// Metrics configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    /// Metric name prefix. When empty (the default), derived from the gear
    /// name by converting it to `snake_case` (e.g., `"oagw"` → `"oagw"`).
    #[serde(default)]
    pub prefix: String,
}

impl MetricsConfig {
    /// Resolve the effective prefix: explicit config value, or
    /// `snake_case(gear_name)`.
    #[must_use]
    pub fn effective_prefix(&self, gear_name: &str) -> String {
        let trimmed = self.prefix.trim();
        if trimmed.is_empty() {
            heck::ToSnakeCase::to_snake_case(gear_name)
        } else {
            trimmed.to_owned()
        }
    }
}

impl Default for OagwConfig {
    fn default() -> Self {
        Self {
            proxy_timeout_secs: default_proxy_timeout_secs(),
            max_body_size_bytes: default_max_body_size_bytes(),
            allow_http_upstream: false,
            token_cache_ttl_secs: default_token_cache_ttl_secs(),
            token_cache_capacity: default_token_cache_capacity(),
            websocket_idle_timeout_secs: default_websocket_idle_timeout_secs(),
            websocket_close_timeout_secs: default_websocket_close_timeout_secs(),
            streaming_idle_timeout_secs: default_streaming_idle_timeout_secs(),
            protocol_cache_ttl_secs: default_protocol_cache_ttl_secs(),
            management_api_enabled: true,
            ssrf_policy: SsrfPolicy::default(),
            metrics: MetricsConfig::default(),
            registry_provisioning: RegistryProvisioningConfig::default(),
            proxy_cache: ProxyCacheConfig::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_proxy_timeout_secs() -> u64 {
    30
}

fn default_max_body_size_bytes() -> usize {
    100 * 1024 * 1024 // 100 MB
}

fn default_token_cache_ttl_secs() -> u64 {
    300 // 5 minutes — acts as a ceiling; actual TTL is min(this, expires_in − 30s)
}

fn default_token_cache_capacity() -> usize {
    10_000
}

fn default_websocket_idle_timeout_secs() -> u64 {
    300 // 5 minutes
}

fn default_websocket_close_timeout_secs() -> u64 {
    5
}

fn default_streaming_idle_timeout_secs() -> u64 {
    300 // 5 minutes — same as websocket idle timeout
}

fn default_protocol_cache_ttl_secs() -> u64 {
    3600 // 1 hour — per spec cpt-cf-oagw-algo-protocol-version-negotiation
}

impl OagwConfig {
    /// Validate configuration values. Returns an error for values that
    /// would cause broken runtime behaviour.
    pub fn validate(&self) -> Result<(), String> {
        if self.websocket_idle_timeout_secs == 0 {
            return Err("websocket_idle_timeout_secs must be > 0".to_owned());
        }
        if self.websocket_close_timeout_secs == 0 {
            return Err("websocket_close_timeout_secs must be > 0".to_owned());
        }
        if self.streaming_idle_timeout_secs == 0 {
            return Err("streaming_idle_timeout_secs must be > 0".to_owned());
        }
        let recheck = self.registry_provisioning.recheck_interval;
        if !(RECHECK_INTERVAL_MIN..=RECHECK_INTERVAL_MAX).contains(&recheck) {
            return Err(format!(
                "registry_provisioning.recheck_interval must be between 1s and 24h, got {recheck:?}"
            ));
        }
        if let Some(generation) = self.registry_provisioning.generation
            && i64::try_from(generation).is_err()
        {
            return Err(format!(
                "registry_provisioning.generation must be at most {} (stored as BIGINT), got {generation}",
                i64::MAX
            ));
        }
        let ttl = self.proxy_cache.ttl;
        if !(PROXY_CACHE_TTL_MIN..=PROXY_CACHE_TTL_MAX).contains(&ttl) {
            return Err(format!(
                "proxy_cache.ttl must be between 1s and 1h, got {ttl:?}"
            ));
        }
        let max_entries = self.proxy_cache.max_entries;
        if max_entries == 0 {
            return Err(format!(
                "proxy_cache.max_entries must be at least 1, got {max_entries}"
            ));
        }
        if max_entries > PROXY_CACHE_MAX_ENTRIES_MAX {
            return Err(format!(
                "proxy_cache.max_entries must be at most {PROXY_CACHE_MAX_ENTRIES_MAX}, got {max_entries}"
            ));
        }
        #[cfg(feature = "fips")]
        if self.allow_http_upstream {
            return Err(
                "allow_http_upstream=true is not permitted under --features fips \
                 (cleartext upstream token endpoints would be rejected by toolkit-http)"
                    .to_owned(),
            );
        }
        Ok(())
    }
}

/// Read-only runtime configuration exposed to handlers via `AppState`.
///
/// Derived from [`OagwConfig`] at init time.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub max_body_size_bytes: usize,
    pub websocket_idle_timeout_secs: u64,
    pub websocket_close_timeout_secs: u64,
    pub streaming_idle_timeout_secs: u64,
    pub management_api_enabled: bool,
}

impl From<&OagwConfig> for RuntimeConfig {
    fn from(cfg: &OagwConfig) -> Self {
        Self {
            max_body_size_bytes: cfg.max_body_size_bytes,
            websocket_idle_timeout_secs: cfg.websocket_idle_timeout_secs,
            websocket_close_timeout_secs: cfg.websocket_close_timeout_secs,
            streaming_idle_timeout_secs: cfg.streaming_idle_timeout_secs,
            management_api_enabled: cfg.management_api_enabled,
        }
    }
}

/// Bundled cache configuration for the OAuth2 token cache.
#[derive(Debug, Clone)]
pub struct TokenCacheConfig {
    pub ttl: Duration,
    pub capacity: usize,
}

impl Default for TokenCacheConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(default_token_cache_ttl_secs()),
            capacity: default_token_cache_capacity(),
        }
    }
}

impl From<&OagwConfig> for TokenCacheConfig {
    fn from(cfg: &OagwConfig) -> Self {
        Self {
            ttl: Duration::from_secs(cfg.token_cache_ttl_secs),
            capacity: cfg.token_cache_capacity,
        }
    }
}

impl fmt::Debug for OagwConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OagwConfig")
            .field("proxy_timeout_secs", &self.proxy_timeout_secs)
            .field("max_body_size_bytes", &self.max_body_size_bytes)
            .field("allow_http_upstream", &self.allow_http_upstream)
            .field("token_cache_ttl_secs", &self.token_cache_ttl_secs)
            .field("token_cache_capacity", &self.token_cache_capacity)
            .field(
                "websocket_idle_timeout_secs",
                &self.websocket_idle_timeout_secs,
            )
            .field(
                "websocket_close_timeout_secs",
                &self.websocket_close_timeout_secs,
            )
            .field(
                "streaming_idle_timeout_secs",
                &self.streaming_idle_timeout_secs,
            )
            .field("protocol_cache_ttl_secs", &self.protocol_cache_ttl_secs)
            .field("management_api_enabled", &self.management_api_enabled)
            .field("ssrf_policy", &self.ssrf_policy)
            .field("registry_provisioning", &self.registry_provisioning)
            .field("proxy_cache", &self.proxy_cache)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_shows_timeout_and_body_size() {
        let config = OagwConfig::default();
        let debug_output = format!("{config:?}");
        assert!(debug_output.contains("proxy_timeout_secs"));
        assert!(debug_output.contains("max_body_size_bytes"));
    }

    #[test]
    fn token_cache_ttl_defaults_to_300() {
        let config = OagwConfig::default();
        assert_eq!(config.token_cache_ttl_secs, 300);
    }

    #[test]
    fn token_cache_capacity_defaults_to_10000() {
        let config = OagwConfig::default();
        assert_eq!(config.token_cache_capacity, 10_000);
    }

    #[test]
    fn validate_rejects_zero_idle_timeout() {
        let config = OagwConfig {
            websocket_idle_timeout_secs: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_close_timeout() {
        let config = OagwConfig {
            websocket_close_timeout_secs: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_accepts_nonzero_timeouts() {
        let config = OagwConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn protocol_cache_ttl_defaults_to_3600() {
        let config = OagwConfig::default();
        assert_eq!(config.protocol_cache_ttl_secs, 3600);
    }

    #[test]
    fn streaming_idle_timeout_defaults_to_300() {
        let config = OagwConfig::default();
        assert_eq!(config.streaming_idle_timeout_secs, 300);
    }

    #[test]
    fn validate_rejects_zero_streaming_idle_timeout() {
        let config = OagwConfig {
            streaming_idle_timeout_secs: 0,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_accepts_zero_protocol_cache_ttl() {
        let config = OagwConfig {
            protocol_cache_ttl_secs: 0,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn registry_provisioning_defaults_to_no_generation_and_five_minutes() {
        let config = OagwConfig::default();
        assert_eq!(config.registry_provisioning.generation, None);
        assert_eq!(
            config.registry_provisioning.recheck_interval,
            Duration::from_mins(5)
        );
    }

    #[test]
    fn registry_provisioning_reads_generation_and_humantime_interval() {
        let config: OagwConfig = serde_json::from_value(serde_json::json!({
            "registry_provisioning": { "generation": 42, "recheck_interval": "30s" }
        }))
        .unwrap();
        assert_eq!(config.registry_provisioning.generation, Some(42));
        assert_eq!(
            config.registry_provisioning.recheck_interval,
            Duration::from_secs(30)
        );
        let unknown = serde_json::from_value::<OagwConfig>(serde_json::json!({
            "registry_provisioning": { "generations": 1 }
        }));
        assert!(unknown.is_err(), "unknown keys are rejected");
    }

    /// ADR-0018: PERS-LOOP-010.
    #[test]
    fn validate_bounds_the_recheck_interval_from_one_second_to_a_day() {
        let with = |interval| {
            let mut config = OagwConfig::default();
            config.registry_provisioning.recheck_interval = interval;
            config.validate()
        };
        for rejected in [
            Duration::ZERO,
            Duration::from_millis(999),
            Duration::from_hours(24) + Duration::from_secs(1),
            Duration::MAX,
        ] {
            let err = with(rejected).expect_err("out of range");
            assert!(
                err.contains("registry_provisioning.recheck_interval"),
                "{err}"
            );
        }
        for accepted in [
            Duration::from_secs(1),
            Duration::from_mins(5),
            Duration::from_hours(24),
        ] {
            assert_eq!(with(accepted), Ok(()));
        }
    }

    #[test]
    fn validate_rejects_a_generation_beyond_bigint() {
        let with = |generation| {
            let mut config = OagwConfig::default();
            config.registry_provisioning.generation = Some(generation);
            config.validate()
        };
        assert_eq!(with(i64::MAX as u64), Ok(()));
        let err = with(i64::MAX as u64 + 1).expect_err("beyond BIGINT");
        assert!(err.contains("registry_provisioning.generation"), "{err}");
    }

    #[test]
    fn proxy_cache_defaults_to_enabled_one_minute_and_ten_thousand_entries() {
        let config: OagwConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(config.proxy_cache.enabled);
        assert_eq!(config.proxy_cache.ttl, Duration::from_secs(60));
        assert_eq!(config.proxy_cache.max_entries, 10_000);
        let partial: OagwConfig = serde_json::from_value(serde_json::json!({
            "proxy_cache": { "enabled": false }
        }))
        .unwrap();
        assert!(!partial.proxy_cache.enabled);
        assert_eq!(
            partial.proxy_cache.ttl,
            Duration::from_secs(60),
            "the rest defaults"
        );
        assert_eq!(partial.proxy_cache.max_entries, 10_000);
    }

    #[test]
    fn proxy_cache_reads_a_humantime_ttl_and_rejects_unknown_keys() {
        let config: OagwConfig = serde_json::from_value(serde_json::json!({
            "proxy_cache": { "enabled": true, "ttl": "2m 30s", "max_entries": 5 }
        }))
        .unwrap();
        assert_eq!(config.proxy_cache.ttl, Duration::from_secs(150));
        assert_eq!(config.proxy_cache.max_entries, 5);
        let unknown = serde_json::from_value::<OagwConfig>(serde_json::json!({
            "proxy_cache": { "ttl_secs": 60 }
        }));
        assert!(unknown.is_err(), "unknown keys are rejected");
    }

    #[test]
    fn validate_bounds_the_proxy_cache_ttl_from_one_second_to_an_hour() {
        let with = |ttl| {
            let mut config = OagwConfig::default();
            config.proxy_cache.ttl = ttl;
            config.validate()
        };
        for rejected in [
            Duration::ZERO,
            Duration::from_millis(999),
            Duration::from_hours(1) + Duration::from_millis(1),
        ] {
            let err = with(rejected).expect_err("out of range");
            assert!(err.contains("proxy_cache.ttl"), "{err}");
            assert!(err.contains(&format!("{rejected:?}")), "{err}");
        }
        for accepted in [
            Duration::from_secs(1),
            Duration::from_mins(1),
            Duration::from_hours(1),
        ] {
            assert_eq!(with(accepted), Ok(()));
        }
    }

    #[test]
    fn validate_needs_at_least_one_proxy_cache_entry() {
        let with = |max_entries| {
            let mut config = OagwConfig::default();
            config.proxy_cache.max_entries = max_entries;
            config.validate()
        };
        let err = with(0).expect_err("no room");
        assert!(err.contains("proxy_cache.max_entries"), "{err}");
        assert!(err.contains("got 0"), "{err}");
        assert_eq!(with(1), Ok(()));
    }

    #[test]
    fn validate_bounds_the_proxy_cache_entries_at_a_million() {
        let with = |max_entries| {
            let mut config = OagwConfig::default();
            config.proxy_cache.max_entries = max_entries;
            config.validate()
        };
        assert_eq!(with(1_000_000), Ok(()));
        let err = with(1_000_001).expect_err("too many");
        assert!(err.contains("proxy_cache.max_entries"), "{err}");
        assert!(err.contains("got 1000001"), "{err}");
    }

    #[cfg(feature = "fips")]
    #[test]
    fn validate_rejects_allow_http_upstream_under_fips() {
        let config = OagwConfig {
            allow_http_upstream: true,
            ..Default::default()
        };
        let err = config
            .validate()
            .expect_err("allow_http_upstream=true must be rejected under --features fips");
        assert!(
            err.contains("allow_http_upstream"),
            "error must mention the offending field, got: {err}"
        );
    }
}
