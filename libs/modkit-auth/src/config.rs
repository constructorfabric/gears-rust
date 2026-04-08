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
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AuthConfig::default();
        assert_eq!(config.leeway_seconds, 60);
        assert!(config.issuers.is_empty());
        assert!(config.audiences.is_empty());
        assert!(config.require_exp);
        assert!(config.jwks.is_none());
    }

    #[test]
    fn test_auth_config_serialization() {
        let config = AuthConfig {
            leeway_seconds: 120,
            issuers: vec!["https://auth.example.com".to_owned()],
            audiences: vec!["api".to_owned()],
            require_exp: true,
            jwks: Some(JwksConfig {
                uri: "https://auth.example.com/.well-known/jwks.json".to_owned(),
                refresh_interval_seconds: 300,
                max_backoff_seconds: 3600,
            }),
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        let deserialized: AuthConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.leeway_seconds, 120);
        assert_eq!(deserialized.issuers, vec!["https://auth.example.com"]);
        assert_eq!(deserialized.audiences, vec!["api"]);
        assert!(deserialized.require_exp);
        let jwks = deserialized.jwks.expect("jwks should be present");
        assert_eq!(jwks.uri, "https://auth.example.com/.well-known/jwks.json");
        assert_eq!(jwks.refresh_interval_seconds, 300);
        assert_eq!(jwks.max_backoff_seconds, 3600);
    }

    #[test]
    fn test_jwks_config_serialization() {
        let config = JwksConfig {
            uri: "https://auth.example.com/.well-known/jwks.json".to_owned(),
            refresh_interval_seconds: 300,
            max_backoff_seconds: 3600,
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        let deserialized: JwksConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.uri, config.uri);
        assert_eq!(
            deserialized.refresh_interval_seconds,
            config.refresh_interval_seconds
        );
        assert_eq!(deserialized.max_backoff_seconds, config.max_backoff_seconds);
    }

    #[test]
    fn test_auth_config_to_validation_config() {
        let auth_config = AuthConfig {
            leeway_seconds: 30,
            issuers: vec!["https://auth.example.com".to_owned()],
            audiences: vec!["api".to_owned()],
            require_exp: true,
            jwks: None,
        };
        let validation_config = ValidationConfig::from(&auth_config);
        assert_eq!(validation_config.allowed_issuers, auth_config.issuers);
        assert_eq!(validation_config.allowed_audiences, auth_config.audiences);
        assert_eq!(validation_config.leeway_seconds, auth_config.leeway_seconds);
        assert!(validation_config.require_exp);
    }

    #[test]
    fn test_require_exp_defaults_true_when_omitted() {
        let json = r#"{"leeway_seconds": 60}"#;
        let config: AuthConfig = serde_json::from_str(json).unwrap();
        assert!(config.require_exp);
    }

    #[test]
    fn test_require_exp_false_propagates_to_validation_config() {
        let auth_config = AuthConfig {
            require_exp: false,
            ..Default::default()
        };
        let validation_config = ValidationConfig::from(&auth_config);
        assert!(!validation_config.require_exp);
    }

    #[test]
    fn test_jwks_config_defaults() {
        let json = r#"{"uri": "https://example.com/jwks"}"#;
        let config: JwksConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.refresh_interval_seconds, 300);
        assert_eq!(config.max_backoff_seconds, 3600);
    }
}
