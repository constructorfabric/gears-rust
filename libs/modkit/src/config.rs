//! Configuration module for typed module configuration access.
//!
//! This module provides two distinct mechanisms for loading module configuration:
//!
//! 1. **Lenient loading** (default): Falls back to `T::default()` when configuration is missing.
//!    - Used by `module_config_or_default`
//!    - Allows modules to exist without configuration sections in the main config file
//!
//! 2. **Strict loading**: Requires configuration to be present and valid.
//!    - Used by `module_config_required`
//!    - Returns errors when configuration is missing or invalid

use serde::de::DeserializeOwned;

/// Configuration error for typed config operations
#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("module '{module}' not found")]
    ModuleNotFound { module: String },
    #[error("module '{module}' config must be an object")]
    InvalidModuleStructure { module: String },
    #[error("missing 'config' section in module '{module}'")]
    MissingConfigSection { module: String },
    #[error("invalid config for module '{module}': {source}")]
    InvalidConfig {
        module: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("variable expansion failed for module '{module}': {source}")]
    VarExpand {
        module: String,
        #[source]
        source: modkit_utils::var_expand::ExpandVarsError,
    },
}

/// Provider of module-specific configuration (raw JSON sections only).
pub trait ConfigProvider: Send + Sync {
    /// Returns raw JSON section for the module, if any.
    fn get_module_config(&self, module_name: &str) -> Option<&serde_json::Value>;
}

/// Lenient configuration loader that falls back to defaults.
///
/// This function provides forgiving behavior for modules that don't require configuration:
/// - If the module is not present in config → returns `Ok(T::default())`
/// - If the module value is not an object → returns `Ok(T::default())`
/// - If the module has no "config" field → returns `Ok(T::default())`
/// - If "config" is present but invalid → returns `Err(ConfigError::InvalidConfig)`
///
/// Use this for modules that can operate with default configuration.
///
/// # Errors
/// Returns `ConfigError::InvalidConfig` if the config section exists but cannot be deserialized.
pub fn module_config_or_default<T: DeserializeOwned + Default>(
    provider: &dyn ConfigProvider,
    module_name: &str,
) -> Result<T, ConfigError> {
    // If module not found, use defaults
    let Some(module_raw) = provider.get_module_config(module_name) else {
        return Ok(T::default());
    };

    // If module is not an object, use defaults
    let Some(obj) = module_raw.as_object() else {
        return Ok(T::default());
    };

    // If no config section, use defaults
    let Some(config_section) = obj.get("config") else {
        return Ok(T::default());
    };

    // Config section exists, try to parse it
    let config: T =
        serde_json::from_value(config_section.clone()).map_err(|e| ConfigError::InvalidConfig {
            module: module_name.to_owned(),
            source: e,
        })?;

    Ok(config)
}

/// Strict configuration loader that requires configuration to be present.
///
/// This function enforces that configuration must exist and be valid:
/// - If the module is not present → returns `Err(ConfigError::ModuleNotFound)`
/// - If the module value is not an object → returns `Err(ConfigError::InvalidModuleStructure)`
/// - If the module has no "config" field → returns `Err(ConfigError::MissingConfigSection)`
/// - If "config" is present but invalid → returns `Err(ConfigError::InvalidConfig)`
///
/// Use this for modules that cannot operate without explicit configuration.
///
/// # Errors
/// Returns `ConfigError` if the module is not found, has invalid structure, or config is invalid.
pub fn module_config_required<T: DeserializeOwned>(
    provider: &dyn ConfigProvider,
    module_name: &str,
) -> Result<T, ConfigError> {
    let module_raw =
        provider
            .get_module_config(module_name)
            .ok_or_else(|| ConfigError::ModuleNotFound {
                module: module_name.to_owned(),
            })?;

    // Extract config section from: modules.<name> = { database: ..., config: ... }
    let obj = module_raw
        .as_object()
        .ok_or_else(|| ConfigError::InvalidModuleStructure {
            module: module_name.to_owned(),
        })?;

    let config_section = obj
        .get("config")
        .ok_or_else(|| ConfigError::MissingConfigSection {
            module: module_name.to_owned(),
        })?;

    let config: T =
        serde_json::from_value(config_section.clone()).map_err(|e| ConfigError::InvalidConfig {
            module: module_name.to_owned(),
            source: e,
        })?;

    Ok(config)
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod tests;
