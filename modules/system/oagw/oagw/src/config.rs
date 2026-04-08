use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};

/// Configuration for the OAGW module.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OagwConfig {
    #[serde(default = "default_proxy_timeout_secs")]
    pub proxy_timeout_secs: u64,
    #[serde(default = "default_max_body_size_bytes")]
    pub max_body_size_bytes: usize,
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
}

impl Default for OagwConfig {
    fn default() -> Self {
        Self {
            proxy_timeout_secs: default_proxy_timeout_secs(),
            max_body_size_bytes: default_max_body_size_bytes(),
            allow_http_upstream: false,
            token_cache_ttl_secs: default_token_cache_ttl_secs(),
            token_cache_capacity: default_token_cache_capacity(),
        }
    }
}

fn default_proxy_timeout_secs() -> u64 {
    30
}

fn default_max_body_size_bytes() -> usize {
    10 * 1024 * 1024 // 10 MB
}

fn default_token_cache_ttl_secs() -> u64 {
    300 // 5 minutes — acts as a ceiling; actual TTL is min(this, expires_in − 30s)
}

fn default_token_cache_capacity() -> usize {
    10_000
}

/// Read-only runtime configuration exposed to handlers via `AppState`.
///
/// Derived from [`OagwConfig`] at init time.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub max_body_size_bytes: usize,
}

impl From<&OagwConfig> for RuntimeConfig {
    fn from(cfg: &OagwConfig) -> Self {
        Self {
            max_body_size_bytes: cfg.max_body_size_bytes,
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
}
