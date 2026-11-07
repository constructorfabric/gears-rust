//! Effective gear configuration dump support.
//!
//! This gear provides utilities for inspecting and dumping the effective
//! runtime configuration of gears, including resolved database DSNs and pool settings.

use super::{AppConfig, RuntimeKind, build_final_db_for_gear, parse_gear_config};
use anyhow::{Context, Result};
use std::path::PathBuf;
use url::Url;

/// List all gear names present in the configuration.
///
/// Returns a sorted vector of gear names that are configured in the `AppConfig`.
/// This is useful for discovering available gears before dumping their configuration.
///
/// # Example
/// ```no_run
/// use toolkit::bootstrap::AppConfig;
/// # fn example(config: &AppConfig) {
/// let gears = toolkit::bootstrap::config::list_gear_names(config);
/// for gear in gears {
///     println!("Gear: {}", gear);
/// }
/// # }
/// ```
#[must_use]
pub fn list_gear_names(app: &AppConfig) -> Vec<String> {
    let mut names: Vec<String> = app.gears.keys().cloned().collect();
    names.sort();
    names
}

/// Render effective configuration for all loaded gears.
///
/// This function builds a complete view of the effective runtime configuration
/// for each gear that is successfully loaded in the `GearRegistry`.
///
/// For each gear, it includes:
/// - `runtime`: Gear runtime type (local/oop) if configured
/// - `config`: Gear-specific configuration section (as-is from config file)
/// - `database`: Final resolved database configuration with redacted DSN (if applicable)
///
/// This is a read-only inspection operation that does not create any directories
/// or modify the filesystem.
///
/// Gears with configuration errors are logged as warnings and skipped,
/// allowing inspection of valid gears even when some are misconfigured.
///
/// # Errors
/// This function does not return errors in practice - all gear-level failures
/// are logged as warnings and the problematic gears are skipped. The `Result`
/// return type is kept for API consistency with other dump functions.
pub fn render_effective_gears_config(app: &AppConfig) -> Result<serde_json::Value> {
    use serde_json::json;

    let home_dir = PathBuf::from(&app.server.home_dir);
    // Prevent path traversal attacks by rejecting paths containing '..'
    if home_dir
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(anyhow::anyhow!("Invalid input: {}", home_dir.display()));
    }
    let mut gears_config = serde_json::Map::new();

    // Iterate over all gears in the configuration
    for gear_name in app.gears.keys() {
        let mut gear_entry = serde_json::Map::new();

        // Parse gear config once for efficiency
        let parsed_config = match parse_gear_config(app, gear_name) {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!(
                    gear =  %gear_name,
                    error = %e,
                    "Failed to parse gear config, skipping"
                );
                continue;
            }
        };

        // Get runtime configuration if present
        if let Some(runtime_config) = parsed_config.runtime {
            gear_entry.insert(
                "runtime".to_owned(),
                json!({
                    "type": match runtime_config.mod_type {
                        RuntimeKind::Local => "local",
                        RuntimeKind::Oop => "oop",
                    }
                }),
            );
        }

        // Get gear config section (the "config" field)
        if !parsed_config.config.is_null() {
            gear_entry.insert("config".to_owned(), parsed_config.config);
        }
        if !parsed_config.clients.is_null() {
            gear_entry.insert("clients".to_owned(), parsed_config.clients);
        }

        // Get database configuration (resolved DSN + pool) - use dry_run=true
        match build_final_db_for_gear(app, gear_name, &home_dir, true) {
            Ok(Some((dsn, pool))) => {
                // Redact password in DSN (warn and skip DB section if this fails, but keep gear)
                let redacted_dsn = match redact_dsn_password(&dsn) {
                    Ok(redacted) => redacted,
                    Err(e) => {
                        tracing::warn!(
                            gear =  %gear_name,
                            error = %e,
                            "Failed to redact DSN password, skipping database config for this gear"
                        );
                        // Continue processing this gear, just skip the DB section
                        // Add gear entry even without DB config
                        if !gear_entry.is_empty() {
                            gears_config.insert(gear_name.clone(), json!(gear_entry));
                        }
                        continue;
                    }
                };

                let mut db_config = serde_json::Map::new();
                db_config.insert("dsn".to_owned(), json!(redacted_dsn));

                // Add pool configuration if present
                let mut pool_map = serde_json::Map::new();
                if let Some(max_conns) = pool.max_conns {
                    pool_map.insert("max_conns".to_owned(), json!(max_conns));
                }
                if let Some(min_conns) = pool.min_conns {
                    pool_map.insert("min_conns".to_owned(), json!(min_conns));
                }
                if let Some(acquire_timeout) = pool.acquire_timeout {
                    pool_map.insert(
                        "acquire_timeout".to_owned(),
                        json!(format!("{}s", acquire_timeout.as_secs())),
                    );
                }
                if let Some(idle_timeout) = pool.idle_timeout {
                    pool_map.insert(
                        "idle_timeout".to_owned(),
                        json!(format!("{}s", idle_timeout.as_secs())),
                    );
                }
                if let Some(max_lifetime) = pool.max_lifetime {
                    pool_map.insert(
                        "max_lifetime".to_owned(),
                        json!(format!("{}s", max_lifetime.as_secs())),
                    );
                }
                if let Some(test_before_acquire) = pool.test_before_acquire {
                    pool_map.insert("test_before_acquire".to_owned(), json!(test_before_acquire));
                }

                if !pool_map.is_empty() {
                    db_config.insert("pool".to_owned(), json!(pool_map));
                }

                gear_entry.insert("database".to_owned(), json!(db_config));
            }
            Ok(None) => {
                // Gear has no database config, skip
            }
            Err(e) => {
                tracing::warn!(
                    gear =  %gear_name,
                    error = %e,
                    "Failed to build database config, skipping"
                );
            }
        }

        // Only add gear to output if it has any configuration
        if !gear_entry.is_empty() {
            gears_config.insert(gear_name.clone(), json!(gear_entry));
        }
    }

    Ok(json!(gears_config))
}

/// Redacts password from a DSN for safe logging.
///
/// Replaces the password portion with `***REDACTED***` while preserving the rest of the DSN.
///
/// # Errors
/// Returns an error if DSN parsing fails.
pub fn redact_dsn_password(dsn: &str) -> Result<String> {
    if dsn.contains('@') {
        let parsed = Url::parse(dsn)?;
        let mut redacted_url = parsed;
        if redacted_url.password().is_some() {
            redacted_url.set_password(Some("***REDACTED***")).ok();
        }
        Ok(redacted_url.to_string())
    } else {
        Ok(dsn.to_owned())
    }
}

/// Dump effective gears configuration as YAML string.
///
/// This function renders the effective configuration for all gears and
/// serializes it to a human-readable YAML format.
///
/// # Errors
/// Returns an error if configuration rendering or YAML serialization fails.
pub fn dump_effective_gears_config_yaml(app: &AppConfig) -> Result<String> {
    let config = render_effective_gears_config(app)?;
    serde_saphyr::to_string(&config).context("Failed to serialize gears configuration to YAML")
}

/// Dump effective gears configuration as JSON string.
///
/// This function renders the effective configuration for all gears and
/// serializes it to a pretty-printed JSON format.
///
/// # Errors
/// Returns an error if configuration rendering or JSON serialization fails.
pub fn dump_effective_gears_config_json(app: &AppConfig) -> Result<String> {
    let config = render_effective_gears_config(app)?;
    serde_json::to_string_pretty(&config).context("Failed to serialize gears configuration to JSON")
}
