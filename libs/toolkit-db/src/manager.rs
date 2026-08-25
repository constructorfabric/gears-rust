//! Database manager for per-gear database connections.
//!
//! The `DbManager` is responsible for:
//! - Loading global database configuration from Figment
//! - Building and caching database handles per gear
//! - Merging global server configurations with gear-specific settings

use crate::config::{DbConnConfig, GlobalDatabaseConfig};
use crate::options::build_db_handle;
use crate::{Db, DbError, Result};
use dashmap::DashMap;
use figment::Figment;
use std::path::{Path, PathBuf};

/// Central database manager that handles per-gear database connections.
pub struct DbManager {
    /// Global database configuration loaded from Figment
    global: Option<GlobalDatabaseConfig>,
    /// Figment instance for reading gear configurations
    figment: Figment,
    /// Base home directory for gears
    home_dir: PathBuf,
    /// Cache of secure DB entrypoints per gear
    cache: DashMap<String, Db>,
}

impl DbManager {
    /// Create a new `DbManager` from a Figment configuration.
    ///
    /// # Errors
    /// Returns an error if the configuration cannot be parsed.
    pub fn from_figment(figment: Figment, home_dir: PathBuf) -> Result<Self> {
        // Parse global database configuration from "db.*" section
        let all_data: serde_json::Value = figment
            .extract()
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));

        let global: Option<GlobalDatabaseConfig> = match all_data.get("database") {
            None => None,
            Some(db) => match serde_json::from_value(db.clone()) {
                Ok(cfg) => Some(cfg),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Global 'database' key is present but failed to deserialize; ignoring"
                    );
                    None
                }
            },
        };

        Ok(Self {
            global,
            figment,
            home_dir,
            cache: DashMap::new(),
        })
    }

    /// Get a database handle for the specified gear.
    /// Returns cached handle if available, otherwise builds a new one.
    ///
    /// # Errors
    /// Returns an error if the database connection cannot be established.
    pub async fn get(&self, gear: &str) -> Result<Option<Db>> {
        // Check cache first
        if let Some(db) = self.cache.get(gear) {
            return Ok(Some(db.clone()));
        }

        // Build new Db
        match self.build_for_gear(gear).await? {
            Some(db) => {
                // Use entry API to handle race conditions properly
                match self.cache.entry(gear.to_owned()) {
                    dashmap::mapref::entry::Entry::Occupied(entry) => {
                        // Another thread beat us to it, return the cached version
                        Ok(Some(entry.get().clone()))
                    }
                    dashmap::mapref::entry::Entry::Vacant(entry) => {
                        // We're first, insert our Db
                        entry.insert(db.clone());
                        Ok(Some(db))
                    }
                }
            }
            _ => Ok(None),
        }
    }

    /// Build a database handle for the specified gear.
    async fn build_for_gear(&self, gear: &str) -> Result<Option<Db>> {
        // Read gear database configuration from Figment
        let gear_data: serde_json::Value = self
            .figment
            .extract()
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));

        let Some(db_value) = gear_data
            .get("gears")
            .and_then(|gears| gears.get(gear))
            .and_then(|m| m.get("database"))
        else {
            tracing::debug!(gear =  %gear, "Gear has no database configuration; skipping");
            return Ok(None);
        };

        let mut cfg: DbConnConfig = match serde_json::from_value(db_value.clone()) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(
                    gear =  %gear,
                    error = %e,
                    "Gear 'database' key is present but failed to deserialize; ignoring"
                );
                return Ok(None);
            }
        };

        // If gear references a global server, merge configurations
        if let Some(server_name) = &cfg.server {
            let server_cfg = self
                .global
                .as_ref()
                .and_then(|g| g.servers.get(server_name))
                .ok_or_else(|| {
                    DbError::InvalidConfig(format!(
                        "Referenced server '{server_name}' not found in global database configuration"
                    ))
                })?;

            cfg = Self::merge_server_into_gear(cfg, server_cfg.clone());
        }

        // Finalize SQLite paths if needed
        let gear_home_dir = self.home_dir.join(gear);
        cfg = self.finalize_sqlite_paths(cfg, &gear_home_dir)?;

        // Build the database handle
        let handle = build_db_handle(cfg, self.global.as_ref()).await?;

        tracing::info!(
            gear =  %gear,
            engine = ?handle.engine(),
            dsn = %crate::options::redact_credentials_in_dsn(Some(handle.dsn())),
            "Built database handle for gear"
        );

        Ok(Some(Db::new(handle)))
    }

    /// Merge global server configuration into gear configuration.
    /// Gear fields override server fields. Params maps are merged with gear taking precedence.
    fn merge_server_into_gear(
        mut gear_cfg: DbConnConfig,
        server_cfg: DbConnConfig,
    ) -> DbConnConfig {
        // Start with server config as base, then apply gear overrides

        // Engine: gear takes precedence (important for field-based configs)
        if gear_cfg.engine.is_none() {
            gear_cfg.engine = server_cfg.engine;
        }

        // DSN: gear takes precedence
        if gear_cfg.dsn.is_none() {
            gear_cfg.dsn = server_cfg.dsn;
        }

        // Individual fields: gear takes precedence
        if gear_cfg.host.is_none() {
            gear_cfg.host = server_cfg.host;
        }
        if gear_cfg.port.is_none() {
            gear_cfg.port = server_cfg.port;
        }
        if gear_cfg.user.is_none() {
            gear_cfg.user = server_cfg.user;
        }
        if gear_cfg.password.is_none() {
            gear_cfg.password = server_cfg.password;
        }
        if gear_cfg.dbname.is_none() {
            gear_cfg.dbname = server_cfg.dbname;
        }

        // Params: merge maps with gear taking precedence
        match (&mut gear_cfg.params, server_cfg.params) {
            (Some(gear_params), Some(server_params)) => {
                // Merge server params first, then gear params (gear overrides)
                for (key, value) in server_params {
                    gear_params.entry(key).or_insert(value);
                }
            }
            (None, Some(server_params)) => {
                gear_cfg.params = Some(server_params);
            }
            _ => {} // Gear has params or server has none - keep gear params
        }

        // Pool: gear takes precedence
        if gear_cfg.pool.is_none() {
            gear_cfg.pool = server_cfg.pool;
        }

        // Lock keepalive: gear takes precedence
        if gear_cfg.lock_keepalive.is_none() {
            gear_cfg.lock_keepalive = server_cfg.lock_keepalive;
        }

        // Note: file, path, and server fields are gear-only and not merged

        gear_cfg
    }

    /// Finalize `SQLite` paths by resolving relative file paths to absolute paths.
    fn finalize_sqlite_paths(
        &self,
        mut cfg: DbConnConfig,
        gear_home: &Path,
    ) -> Result<DbConnConfig> {
        // If file is specified, convert to absolute path under gear home
        if let Some(file) = &cfg.file {
            let absolute_path = gear_home.join(file);

            // Check auto_provision setting
            let auto_provision = self
                .global
                .as_ref()
                .and_then(|g| g.auto_provision)
                .unwrap_or(true);

            if auto_provision {
                // Create all necessary directories
                if let Some(parent) = absolute_path.parent() {
                    std::fs::create_dir_all(parent).map_err(DbError::Io)?;
                }
            } else if let Some(parent) = absolute_path.parent() {
                // When auto_provision is false, check if the directory exists
                if !parent.exists() {
                    return Err(DbError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!(
                            "Directory does not exist and auto_provision is disabled: {}",
                            parent.display()
                        ),
                    )));
                }
            }

            cfg.path = Some(absolute_path);
            cfg.file = None; // Clear file since path takes precedence and we can't have both
        }

        // If path is relative, make it absolute relative to gear home
        if let Some(path) = &cfg.path
            && path.is_relative()
        {
            cfg.path = Some(gear_home.join(path));
        }

        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::{DbConnConfig, DbManager};
    use std::time::Duration;

    /// Direct coverage for the `lock_keepalive` merge branch. Exercised here rather than through
    /// `DbManager::get`, whose `None` case only reaches this code on the way to a connection
    /// failure and so cannot distinguish "inherited" from "never set".
    #[test]
    fn lock_keepalive_is_inherited_from_server_when_gear_leaves_it_unset() {
        let server = DbConnConfig {
            lock_keepalive: Some(Duration::from_secs(7)),
            ..Default::default()
        };
        let gear = DbConnConfig {
            lock_keepalive: None,
            ..Default::default()
        };

        let merged = DbManager::merge_server_into_gear(gear, server);
        assert_eq!(merged.lock_keepalive, Some(Duration::from_secs(7)));
    }

    #[test]
    fn gear_lock_keepalive_overrides_server() {
        let server = DbConnConfig {
            lock_keepalive: Some(Duration::from_secs(7)),
            ..Default::default()
        };
        let gear = DbConnConfig {
            lock_keepalive: Some(Duration::from_millis(250)),
            ..Default::default()
        };

        let merged = DbManager::merge_server_into_gear(gear, server);
        assert_eq!(merged.lock_keepalive, Some(Duration::from_millis(250)));
    }

    /// Neither side setting it must stay `None` so the caller resolves the crate default.
    #[test]
    fn lock_keepalive_stays_unset_when_neither_side_sets_it() {
        let merged =
            DbManager::merge_server_into_gear(DbConnConfig::default(), DbConnConfig::default());
        assert!(merged.lock_keepalive.is_none());
    }
}
