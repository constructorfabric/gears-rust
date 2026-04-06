use crate::validation::ValidationConfig;
use serde::{Deserialize, Serialize};

/// Main authentication configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Leeway in seconds for time-based validations (exp, nbf)
    #[serde(default = "default_leeway")]
    pub leeway_seconds: i64,

    /// Allowed issuers (if empty, any issuer is accepted)
    #[serde(default)]
    pub issuers: Vec<String>,

    /// Allowed audiences (if empty, any audience is accepted)
    #[serde(default)]
    pub audiences: Vec<String>,

    /// Whether the `exp` claim is required (default: `true`).
    /// Set to `false` to allow tokens without an expiration claim.
    #[serde(default = "default_require_exp")]
    pub require_exp: bool,

    /// JWKS configuration
    #[serde(default)]
    pub jwks: Option<JwksConfig>,
}

fn default_leeway() -> i64 {
    60
}

fn default_require_exp() -> bool {
    true
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            leeway_seconds: default_leeway(),
            issuers: Vec::new(),
            audiences: Vec::new(),
            require_exp: default_require_exp(),
            jwks: None,
        }
    }
}

impl From<&AuthConfig> for ValidationConfig {
    fn from(config: &AuthConfig) -> Self {
        Self {
            allowed_issuers: config.issuers.clone(),
            allowed_audiences: config.audiences.clone(),
            leeway_seconds: config.leeway_seconds,
            require_exp: config.require_exp,
        }
    }
}

/// JWKS endpoint configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwksConfig {
    /// JWKS endpoint URL
    pub uri: String,

    /// Refresh interval in seconds (default: 300 = 5 minutes)
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_seconds: u64,

    /// Maximum backoff in seconds (default: 3600 = 1 hour)
    #[serde(default = "default_max_backoff")]
    pub max_backoff_seconds: u64,
}

fn default_refresh_interval() -> u64 {
    300
}

fn default_max_backoff() -> u64 {
    3600
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod tests;
