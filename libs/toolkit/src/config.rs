//! Configuration gear for typed gear configuration access.
//!
//! This gear provides two distinct mechanisms for loading gear configuration:
//!
//! 1. **Lenient loading** (default): Falls back to `T::default()` when configuration is missing.
//!    - Used by `gear_config_or_default`
//!    - Allows gears to exist without configuration sections in the main config file
//!
//! 2. **Strict loading**: Requires configuration to be present and valid.
//!    - Used by `gear_config_required`
//!    - Returns errors when configuration is missing or invalid

use serde::de::DeserializeOwned;

/// Configuration error for typed config operations
#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("gear '{gear}' not found")]
    GearNotFound { gear: String },
    #[error("gear '{gear}' config must be an object")]
    InvalidGearStructure { gear: String },
    #[error("missing 'config' section in gear '{gear}'")]
    MissingConfigSection { gear: String },
    // Intentionally not named `source`; doing so would duplicate chained error output.
    #[error("invalid config for gear '{gear}': {cause}")]
    InvalidConfig {
        gear: String,
        cause: serde_json::Error,
    },
    #[error("variable expansion failed for gear '{gear}': {cause}")]
    VarExpand {
        gear: String,
        cause: toolkit_utils::var_expand::ExpandVarsError,
    },
}

/// Provider of gear-specific configuration (raw JSON sections only).
pub trait ConfigProvider: Send + Sync {
    /// Returns raw JSON section for the gear, if any.
    fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value>;
}

/// Lenient configuration loader that falls back to defaults.
///
/// This function provides forgiving behavior for gears that don't require configuration:
/// - If the gear is not present in config → returns `Ok(T::default())`
/// - If the gear value is not an object → returns `Ok(T::default())`
/// - If the gear has no "config" field → returns `Ok(T::default())`
/// - If "config" is present but invalid → returns `Err(ConfigError::InvalidConfig)`
///
/// Use this for gears that can operate with default configuration.
///
/// # Errors
/// Returns `ConfigError::InvalidConfig` if the config section exists but cannot be deserialized.
pub fn gear_config_or_default<T: DeserializeOwned + Default>(
    provider: &dyn ConfigProvider,
    gear_name: &str,
) -> Result<T, ConfigError> {
    // If gear not found, use defaults
    let Some(gear_raw) = provider.get_gear_config(gear_name) else {
        return Ok(T::default());
    };

    // If gear is not an object, use defaults
    let Some(obj) = gear_raw.as_object() else {
        return Ok(T::default());
    };

    // If no config section, use defaults
    let Some(config_section) = obj.get("config") else {
        return Ok(T::default());
    };

    // Config section exists, try to parse it
    let config: T =
        serde_json::from_value(config_section.clone()).map_err(|e| ConfigError::InvalidConfig {
            gear: gear_name.to_owned(),
            cause: e,
        })?;

    Ok(config)
}

/// Strict configuration loader that requires configuration to be present.
///
/// This function enforces that configuration must exist and be valid:
/// - If the gear is not present → returns `Err(ConfigError::GearNotFound)`
/// - If the gear value is not an object → returns `Err(ConfigError::InvalidGearStructure)`
/// - If the gear has no "config" field → returns `Err(ConfigError::MissingConfigSection)`
/// - If "config" is present but invalid → returns `Err(ConfigError::InvalidConfig)`
///
/// Use this for gears that cannot operate without explicit configuration.
///
/// # Errors
/// Returns `ConfigError` if the gear is not found, has invalid structure, or config is invalid.
pub fn gear_config_required<T: DeserializeOwned>(
    provider: &dyn ConfigProvider,
    gear_name: &str,
) -> Result<T, ConfigError> {
    let gear_raw =
        provider
            .get_gear_config(gear_name)
            .ok_or_else(|| ConfigError::GearNotFound {
                gear: gear_name.to_owned(),
            })?;

    // Extract config section from: gears.<name> = { database: ..., config: ... }
    let obj = gear_raw
        .as_object()
        .ok_or_else(|| ConfigError::InvalidGearStructure {
            gear: gear_name.to_owned(),
        })?;

    let config_section = obj
        .get("config")
        .ok_or_else(|| ConfigError::MissingConfigSection {
            gear: gear_name.to_owned(),
        })?;

    let config: T =
        serde_json::from_value(config_section.clone()).map_err(|e| ConfigError::InvalidConfig {
            gear: gear_name.to_owned(),
            cause: e,
        })?;

    Ok(config)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;
    use std::collections::HashMap;

    #[derive(Debug, PartialEq, Deserialize, Default)]
    struct TestConfig {
        #[serde(default)]
        api_key: String,
        #[serde(default)]
        timeout_ms: u64,
        #[serde(default)]
        enabled: bool,
    }

    struct MockConfigProvider {
        gears: HashMap<String, serde_json::Value>,
    }

    impl MockConfigProvider {
        fn new() -> Self {
            let mut gears = HashMap::new();

            // Valid gear config
            gears.insert(
                "test_gear".to_owned(),
                json!({
                    "database": {
                        "url": "postgres://localhost/test"
                    },
                    "config": {
                        "api_key": "secret123",
                        "timeout_ms": 5000,
                        "enabled": true
                    }
                }),
            );

            // Gear without config section
            gears.insert(
                "no_config_gear".to_owned(),
                json!({
                    "database": {
                        "url": "postgres://localhost/test"
                    }
                }),
            );

            // Gear with invalid structure (not an object)
            gears.insert("invalid_gear".to_owned(), json!("not an object"));

            Self { gears }
        }
    }

    impl ConfigProvider for MockConfigProvider {
        fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
            self.gears.get(gear_name)
        }
    }

    // ========== Tests for lenient loading (gear_config_or_default) ==========

    #[test]
    fn test_lenient_success() {
        let provider = MockConfigProvider::new();
        let result: Result<TestConfig, ConfigError> =
            gear_config_or_default(&provider, "test_gear");

        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.api_key, "secret123");
        assert_eq!(config.timeout_ms, 5000);
        assert!(config.enabled);
    }

    #[test]
    fn test_lenient_gear_not_found_returns_default() {
        let provider = MockConfigProvider::new();
        let result: Result<TestConfig, ConfigError> =
            gear_config_or_default(&provider, "nonexistent");

        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config, TestConfig::default());
    }

    #[test]
    fn test_lenient_missing_config_section_returns_default() {
        let provider = MockConfigProvider::new();
        let result: Result<TestConfig, ConfigError> =
            gear_config_or_default(&provider, "no_config_gear");

        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config, TestConfig::default());
    }

    #[test]
    fn test_lenient_invalid_structure_returns_default() {
        let provider = MockConfigProvider::new();
        let result: Result<TestConfig, ConfigError> =
            gear_config_or_default(&provider, "invalid_gear");

        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config, TestConfig::default());
    }

    #[test]
    fn test_lenient_invalid_config_returns_error() {
        let mut provider = MockConfigProvider::new();
        // Add gear with invalid config structure
        provider.gears.insert(
            "bad_config_gear".to_owned(),
            json!({
                "config": {
                    "api_key": "secret123",
                    "timeout_ms": "not_a_number", // Should be u64
                    "enabled": true
                }
            }),
        );

        let result: Result<TestConfig, ConfigError> =
            gear_config_or_default(&provider, "bad_config_gear");

        assert!(matches!(result, Err(ConfigError::InvalidConfig { .. })));
        if let Err(ConfigError::InvalidConfig { gear, .. }) = result {
            assert_eq!(gear, "bad_config_gear");
        }
    }

    #[test]
    fn test_lenient_helper_with_multiple_scenarios() {
        let provider = MockConfigProvider::new();

        // Gear not found should return default
        let result: Result<TestConfig, ConfigError> =
            gear_config_or_default(&provider, "nonexistent");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TestConfig::default());

        // Valid config should parse correctly
        let result: Result<TestConfig, ConfigError> =
            gear_config_or_default(&provider, "test_gear");
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.api_key, "secret123");
    }

    // ========== Tests for strict loading (gear_config_required) ==========

    #[test]
    fn test_strict_success() {
        let provider = MockConfigProvider::new();
        let result: Result<TestConfig, ConfigError> = gear_config_required(&provider, "test_gear");

        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.api_key, "secret123");
        assert_eq!(config.timeout_ms, 5000);
        assert!(config.enabled);
    }

    #[test]
    fn test_strict_gear_not_found() {
        let provider = MockConfigProvider::new();
        let result: Result<TestConfig, ConfigError> =
            gear_config_required(&provider, "nonexistent");

        assert!(matches!(result, Err(ConfigError::GearNotFound { .. })));
        if let Err(ConfigError::GearNotFound { gear }) = result {
            assert_eq!(gear, "nonexistent");
        }
    }

    #[test]
    fn test_strict_missing_config_section() {
        let provider = MockConfigProvider::new();
        let result: Result<TestConfig, ConfigError> =
            gear_config_required(&provider, "no_config_gear");

        assert!(matches!(
            result,
            Err(ConfigError::MissingConfigSection { .. })
        ));
        if let Err(ConfigError::MissingConfigSection { gear }) = result {
            assert_eq!(gear, "no_config_gear");
        }
    }

    #[test]
    fn test_strict_invalid_structure() {
        let provider = MockConfigProvider::new();
        let result: Result<TestConfig, ConfigError> =
            gear_config_required(&provider, "invalid_gear");

        assert!(matches!(
            result,
            Err(ConfigError::InvalidGearStructure { .. })
        ));
        if let Err(ConfigError::InvalidGearStructure { gear }) = result {
            assert_eq!(gear, "invalid_gear");
        }
    }

    #[test]
    fn test_strict_invalid_config() {
        let mut provider = MockConfigProvider::new();
        // Add gear with invalid config structure
        provider.gears.insert(
            "bad_config_gear".to_owned(),
            json!({
                "config": {
                    "api_key": "secret123",
                    "timeout_ms": "not_a_number", // Should be u64
                    "enabled": true
                }
            }),
        );

        let result: Result<TestConfig, ConfigError> =
            gear_config_required(&provider, "bad_config_gear");

        assert!(matches!(result, Err(ConfigError::InvalidConfig { .. })));
        if let Err(ConfigError::InvalidConfig { gear, .. }) = result {
            assert_eq!(gear, "bad_config_gear");
        }
    }

    // ========== Tests for ConfigError display messages ==========

    #[test]
    fn test_config_error_messages() {
        let gear_not_found = ConfigError::GearNotFound {
            gear: "test".to_owned(),
        };
        assert_eq!(gear_not_found.to_string(), "gear 'test' not found");

        let invalid_structure = ConfigError::InvalidGearStructure {
            gear: "test".to_owned(),
        };
        assert_eq!(
            invalid_structure.to_string(),
            "gear 'test' config must be an object"
        );

        let missing_config = ConfigError::MissingConfigSection {
            gear: "test".to_owned(),
        };
        assert_eq!(
            missing_config.to_string(),
            "missing 'config' section in gear 'test'"
        );
    }

    #[test]
    fn test_invalid_config_message_is_not_duplicated_by_source_chain() {
        let mut provider = MockConfigProvider::new();
        provider.gears.insert(
            "bad_config_gear".to_owned(),
            json!({
                "config": {
                    "api_key": "secret123",
                    "timeout_ms": "not_a_number",
                    "enabled": true
                }
            }),
        );
        let result: Result<TestConfig, ConfigError> =
            gear_config_required(&provider, "bad_config_gear");
        let err = result.unwrap_err();

        assert_eq!(
            err.to_string(),
            "invalid config for gear 'bad_config_gear': invalid type: string \"not_a_number\", expected u64"
        );

        assert!(std::error::Error::source(&err).is_none());
        let anyhow_err: anyhow::Error = err.into();
        assert_eq!(
            format!("{anyhow_err:#}"),
            "invalid config for gear 'bad_config_gear': invalid type: string \"not_a_number\", expected u64"
        );
    }

    #[test]
    fn test_var_expand_message_is_not_duplicated_by_source_chain() {
        let err = ConfigError::VarExpand {
            gear: "test".to_owned(),
            cause: toolkit_utils::var_expand::ExpandVarsError::Regex("boom".to_owned()),
        };

        assert_eq!(
            err.to_string(),
            "variable expansion failed for gear 'test': env expansion regex error: boom"
        );
        assert!(std::error::Error::source(&err).is_none());
    }
}
