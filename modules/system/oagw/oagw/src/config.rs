use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};

use crate::domain::ssrf::SsrfPolicy;

/// Configuration for the OAGW module.
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
    /// be refused by `modkit-http` on the first proxied request.
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
    /// Timeout in seconds for the WebSocket Close frame handshake.
    /// After sending or forwarding a Close frame, the gateway waits this long
    /// for the Close response before force-closing. Must be > 0. Default: 5.
    #[serde(default = "default_websocket_close_timeout_secs")]
    pub websocket_close_timeout_secs: u64,
    /// Optional maximum WebSocket frame payload size in bytes.
    /// Frames exceeding this limit trigger Close frame 1009 (Message Too Big).
    /// Default: None (pass-through, no limit enforced).
    #[serde(default)]
    pub websocket_max_frame_size_bytes: Option<usize>,
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
}

/// Metrics configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    /// Metric name prefix. When empty (the default), derived from the module
    /// name by converting it to `snake_case` (e.g., `"oagw"` → `"oagw"`).
    #[serde(default)]
    pub prefix: String,
}

impl MetricsConfig {
    /// Resolve the effective prefix: explicit config value, or
    /// `snake_case(module_name)`.
    #[must_use]
    pub fn effective_prefix(&self, module_name: &str) -> String {
        let trimmed = self.prefix.trim();
        if trimmed.is_empty() {
            heck::ToSnakeCase::to_snake_case(module_name)
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
            websocket_max_frame_size_bytes: None,
            streaming_idle_timeout_secs: default_streaming_idle_timeout_secs(),
            protocol_cache_ttl_secs: default_protocol_cache_ttl_secs(),
            management_api_enabled: true,
            ssrf_policy: SsrfPolicy::default(),
            metrics: MetricsConfig::default(),
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
        #[cfg(feature = "fips")]
        if self.allow_http_upstream {
            return Err(
                "allow_http_upstream=true is not permitted under --features fips \
                 (cleartext upstream token endpoints would be rejected by modkit-http)"
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
    pub websocket_max_frame_size_bytes: Option<usize>,
    pub streaming_idle_timeout_secs: u64,
    pub management_api_enabled: bool,
}

impl From<&OagwConfig> for RuntimeConfig {
    fn from(cfg: &OagwConfig) -> Self {
        Self {
            max_body_size_bytes: cfg.max_body_size_bytes,
            websocket_idle_timeout_secs: cfg.websocket_idle_timeout_secs,
            websocket_close_timeout_secs: cfg.websocket_close_timeout_secs,
            websocket_max_frame_size_bytes: cfg.websocket_max_frame_size_bytes,
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
                "websocket_max_frame_size_bytes",
                &self.websocket_max_frame_size_bytes,
            )
            .field(
                "streaming_idle_timeout_secs",
                &self.streaming_idle_timeout_secs,
            )
            .field("protocol_cache_ttl_secs", &self.protocol_cache_ttl_secs)
            .field("management_api_enabled", &self.management_api_enabled)
            .field("ssrf_policy", &self.ssrf_policy)
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
