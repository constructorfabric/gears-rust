//! Tests for `OoP` configuration merge logic
//!
//! Tests cover all merge scenarios:
//! - Database: field-by-field merge (global.servers → gear.database in master → gear.database in local)
//! - Logging: key-by-key merge (local keys override master keys)
//! - Config: full replacement (local replaces master if present)

use super::*;
use crate::bootstrap::config::{
    AppConfig, ConsoleFormat, GlobalDatabaseConfig, LoggingConfig, RenderedDbConfig,
    RenderedGearConfig, Section, SectionFile, ServerConfig, default_logging_config,
};
use std::collections::HashMap;
use std::time::Duration;
use toolkit_db::{DbConnConfig, PoolCfg};
use tracing::Level;

/// Helper to create a minimal `AppConfig` for testing
fn minimal_app_config() -> AppConfig {
    AppConfig {
        server: ServerConfig {
            home_dir: std::env::temp_dir().join("toolkit_test"),
            ..Default::default()
        },
        logging: default_logging_config(),
        ..Default::default()
    }
}

/// Helper to create a logging section
fn logging_section(console_level: Option<Level>, file: &str) -> Section {
    Section {
        console_level,
        section_file: Some(SectionFile {
            file: file.to_owned(),
            file_level: Some(Level::DEBUG),
        }),
        console_format: ConsoleFormat::default(),
        max_age_days: Some(7),
        max_backups: Some(3),
        max_size_mb: Some(100),
    }
}

// =============================================================================
// Logging Merge Tests
// =============================================================================

mod logging_merge {
    use super::*;

    #[test]
    fn test_merge_logging_local_only() {
        // When only local has logging, result should be local's logging
        let local_logging: LoggingConfig = [(
            "default".to_owned(),
            logging_section(Some(Level::DEBUG), "logs/local.log"),
        )]
        .into();

        let result = merge_logging_configs(None, &local_logging);

        assert_eq!(result.len(), 1);
        assert_eq!(
            result.get("default").unwrap().console_level,
            Some(Level::DEBUG)
        );
        assert_eq!(
            result.get("default").unwrap().file().unwrap(),
            "logs/local.log"
        );
    }

    #[test]
    fn test_merge_logging_local_overrides_master_key() {
        // Local key should override master key
        let master_logging: LoggingConfig = [
            (
                "default".to_owned(),
                logging_section(Some(Level::INFO), "logs/master.log"),
            ),
            (
                "gear_a".to_owned(),
                logging_section(Some(Level::INFO), "logs/a-master.log"),
            ),
        ]
        .into();

        let local_logging: LoggingConfig = [(
            "default".to_owned(),
            logging_section(Some(Level::DEBUG), "logs/local.log"),
        )]
        .into();

        let result = merge_logging_configs(Some(&master_logging), &local_logging);

        assert_eq!(result.len(), 2);
        // Local overrides default
        assert_eq!(
            result.get("default").unwrap().console_level,
            Some(Level::DEBUG)
        );
        assert_eq!(
            result.get("default").unwrap().file().unwrap(),
            "logs/local.log"
        );
        // Master's gear_a preserved
        assert_eq!(
            result.get("gear_a").unwrap().console_level,
            Some(Level::INFO)
        );
        assert_eq!(
            result.get("gear_a").unwrap().file().unwrap(),
            "logs/a-master.log"
        );
    }

    #[test]
    fn test_merge_logging_local_adds_new_key() {
        // Local can add new keys that don't exist in master
        let master_logging: LoggingConfig = [(
            "default".to_owned(),
            logging_section(Some(Level::INFO), "logs/default.log"),
        )]
        .into();

        let local_logging: LoggingConfig = [(
            "new_gear".to_owned(),
            logging_section(Some(Level::TRACE), "logs/new.log"),
        )]
        .into();

        let result = merge_logging_configs(Some(&master_logging), &local_logging);

        assert_eq!(result.len(), 2);
        assert_eq!(
            result.get("default").unwrap().console_level,
            Some(Level::INFO)
        );
        assert_eq!(
            result.get("new_gear").unwrap().console_level,
            Some(Level::TRACE)
        );
    }

    #[test]
    fn test_merge_logging_multiple_overrides() {
        // Multiple keys can be overridden
        let master_logging: LoggingConfig = [
            (
                "default".to_owned(),
                logging_section(Some(Level::INFO), "logs/default.log"),
            ),
            (
                "sqlx".to_owned(),
                logging_section(Some(Level::WARN), "logs/sql.log"),
            ),
            (
                "api".to_owned(),
                logging_section(Some(Level::INFO), "logs/api.log"),
            ),
        ]
        .into();

        let local_logging: LoggingConfig = [
            (
                "default".to_owned(),
                logging_section(Some(Level::DEBUG), "logs/local-default.log"),
            ),
            (
                "sqlx".to_owned(),
                logging_section(Some(Level::DEBUG), "logs/local-sql.log"),
            ),
        ]
        .into();

        let result = merge_logging_configs(Some(&master_logging), &local_logging);

        assert_eq!(result.len(), 3);
        // Overridden
        assert_eq!(
            result.get("default").unwrap().console_level,
            Some(Level::DEBUG)
        );
        assert_eq!(
            result.get("sqlx").unwrap().console_level,
            Some(Level::DEBUG)
        );
        // Preserved from master
        assert_eq!(result.get("api").unwrap().console_level, Some(Level::INFO));
    }
}

// =============================================================================
// JSON Object Merge Tests
// =============================================================================

mod json_merge {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_merge_json_flat_objects() {
        let mut target = json!({"a": 1, "b": 2});
        let source = json!({"b": 3, "c": 4});

        merge_json_objects(&mut target, &source);

        assert_eq!(target, json!({"a": 1, "b": 3, "c": 4}));
    }

    #[test]
    fn test_merge_json_nested_objects() {
        let mut target = json!({
            "database": {
                "host": "localhost",
                "port": 5432
            }
        });
        let source = json!({
            "database": {
                "port": 5433,
                "user": "admin"
            }
        });

        merge_json_objects(&mut target, &source);

        assert_eq!(
            target,
            json!({
                "database": {
                    "host": "localhost",
                    "port": 5433,
                    "user": "admin"
                }
            })
        );
    }

    #[test]
    fn test_merge_json_deeply_nested() {
        let mut target = json!({
            "level1": {
                "level2": {
                    "a": 1,
                    "b": 2
                }
            }
        });
        let source = json!({
            "level1": {
                "level2": {
                    "b": 3,
                    "c": 4
                },
                "new_key": "value"
            }
        });

        merge_json_objects(&mut target, &source);

        assert_eq!(
            target,
            json!({
                "level1": {
                    "level2": {
                        "a": 1,
                        "b": 3,
                        "c": 4
                    },
                    "new_key": "value"
                }
            })
        );
    }

    #[test]
    fn test_merge_json_source_replaces_non_object() {
        // When target has non-object value, source object replaces it
        let mut target = json!({"key": "string_value"});
        let source = json!({"key": {"nested": true}});

        merge_json_objects(&mut target, &source);

        assert_eq!(target, json!({"key": {"nested": true}}));
    }

    #[test]
    fn test_merge_json_non_object_replaces_object() {
        // When source has non-object value, it replaces target object
        let mut target = json!({"key": {"nested": true}});
        let source = json!({"key": "string_value"});

        merge_json_objects(&mut target, &source);

        assert_eq!(target, json!({"key": "string_value"}));
    }

    #[test]
    fn test_merge_json_empty_source() {
        let mut target = json!({"a": 1, "b": 2});
        let source = json!({});

        merge_json_objects(&mut target, &source);

        assert_eq!(target, json!({"a": 1, "b": 2}));
    }

    #[test]
    fn test_merge_json_empty_target() {
        let mut target = json!({});
        let source = json!({"a": 1, "b": 2});

        merge_json_objects(&mut target, &source);

        assert_eq!(target, json!({"a": 1, "b": 2}));
    }
}

// =============================================================================
// Database Merge Tests (via build_merged_db_options)
// =============================================================================

mod database_merge {
    use super::*;
    use serde_json::json;

    fn create_global_db_config() -> GlobalDatabaseConfig {
        let mut servers = HashMap::new();
        servers.insert(
            "sqlite_main".to_owned(),
            DbConnConfig {
                engine: Some(toolkit_db::config::DbEngineCfg::Sqlite),
                server: None,
                dsn: None,
                host: None,
                port: None,
                user: None,
                password: None,
                dbname: None,
                file: None,
                path: None,
                params: Some([("WAL".to_owned(), "true".to_owned())].into()),
                pool: Some(PoolCfg {
                    max_conns: Some(5),
                    min_conns: None,
                    acquire_timeout: Some(Duration::from_secs(30)),
                    idle_timeout: None,
                    max_lifetime: None,
                    test_before_acquire: None,
                }),
                lock_keepalive: None,
            },
        );
        GlobalDatabaseConfig {
            servers,
            auto_provision: Some(true),
        }
    }

    fn create_gear_db_config() -> DbConnConfig {
        DbConnConfig {
            engine: Some(toolkit_db::config::DbEngineCfg::Sqlite),
            server: Some("sqlite_main".to_owned()),
            dsn: None,
            host: None,
            port: None,
            user: None,
            password: None,
            dbname: None,
            file: Some("gear.db".to_owned()),
            path: None,
            params: None,
            pool: None,
            lock_keepalive: None,
        }
    }

    #[test]
    fn test_rendered_db_config_no_database() {
        // When no database config, result should be DbOptions::None
        let home_dir = std::env::temp_dir().join("toolkit_test_no_db");
        let local_config = minimal_app_config();

        let result = build_merged_db_options(&home_dir, "test_gear", None, &local_config);

        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), DbOptions::None));
    }

    #[test]
    fn test_rendered_db_config_master_only() {
        // When only master has database config
        let home_dir = std::env::temp_dir().join("toolkit_test_master_only");
        _ = std::fs::create_dir_all(&home_dir);

        let rendered_db = RenderedDbConfig::new(
            Some(create_global_db_config()),
            Some(create_gear_db_config()),
        );

        let local_config = minimal_app_config();

        let result =
            build_merged_db_options(&home_dir, "test_gear", Some(&rendered_db), &local_config);

        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), DbOptions::Manager(_)));
    }

    #[test]
    fn test_rendered_db_config_local_only() {
        // When only local has database config (standalone mode)
        let home_dir = std::env::temp_dir().join("toolkit_test_local_only");
        _ = std::fs::create_dir_all(&home_dir);

        let mut local_config = minimal_app_config();
        local_config.database = Some(create_global_db_config());
        local_config.gears.insert(
            "test_gear".to_owned(),
            json!({
                "database": {
                    "server": "sqlite_main",
                    "file": "local.db"
                }
            }),
        );

        let result = build_merged_db_options(&home_dir, "test_gear", None, &local_config);

        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), DbOptions::Manager(_)));
    }

    /// A unit co-hosting gears gives each one naming a database in the local config
    /// its own database, not only the unit's gear.
    #[tokio::test]
    async fn test_local_db_config_reaches_co_hosted_gears() {
        let home_dir = tempfile::tempdir().expect("temp dir");

        let mut local_config = minimal_app_config();
        local_config.database = Some(create_global_db_config());
        for (gear, file) in [("test_gear", "own.db"), ("co_hosted", "co_hosted.db")] {
            local_config.gears.insert(
                gear.to_owned(),
                json!({ "database": { "server": "sqlite_main", "file": file } }),
            );
        }
        local_config
            .gears
            .insert("no_database".to_owned(), json!({ "config": {} }));

        let DbOptions::Manager(manager) =
            build_merged_db_options(home_dir.path(), "test_gear", None, &local_config)
                .expect("db options")
        else {
            panic!("a database manager");
        };

        for gear in ["test_gear", "co_hosted"] {
            assert!(
                manager.get(gear).await.expect("resolves").is_some(),
                "{gear} gets its database"
            );
        }
        assert!(
            manager
                .get("no_database")
                .await
                .expect("resolves")
                .is_none()
        );
    }

    #[test]
    fn test_rendered_db_config_local_overrides_pool() {
        // Local config should override pool settings from master
        let home_dir = std::env::temp_dir().join("toolkit_test_pool_override");
        _ = std::fs::create_dir_all(&home_dir);

        let rendered_db = RenderedDbConfig::new(
            Some(create_global_db_config()),
            Some(create_gear_db_config()),
        );

        let mut local_config = minimal_app_config();
        // Local overrides pool.max_conns
        local_config.gears.insert(
            "test_gear".to_owned(),
            json!({
                "database": {
                    "pool": {
                        "max_conns": 10
                    }
                }
            }),
        );

        let result =
            build_merged_db_options(&home_dir, "test_gear", Some(&rendered_db), &local_config);

        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), DbOptions::Manager(_)));
    }

    #[test]
    fn test_rendered_db_config_local_overrides_file() {
        // Local config should override file path from master
        let home_dir = std::env::temp_dir().join("toolkit_test_file_override");
        _ = std::fs::create_dir_all(&home_dir);

        let rendered_db = RenderedDbConfig::new(
            Some(create_global_db_config()),
            Some(create_gear_db_config()),
        );

        let mut local_config = minimal_app_config();
        // Local overrides file
        local_config.gears.insert(
            "test_gear".to_owned(),
            json!({
                "database": {
                    "file": "local_override.db"
                }
            }),
        );

        let result =
            build_merged_db_options(&home_dir, "test_gear", Some(&rendered_db), &local_config);

        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), DbOptions::Manager(_)));
    }

    #[test]
    fn test_rendered_db_config_local_adds_params() {
        // Local config can add new params to master's params
        let home_dir = std::env::temp_dir().join("toolkit_test_params_add");
        _ = std::fs::create_dir_all(&home_dir);

        let rendered_db = RenderedDbConfig::new(
            Some(create_global_db_config()),
            Some(create_gear_db_config()),
        );

        let mut local_config = minimal_app_config();
        // Local adds new params
        local_config.gears.insert(
            "test_gear".to_owned(),
            json!({
                "database": {
                    "params": {
                        "new_param": "value"
                    }
                }
            }),
        );

        let result =
            build_merged_db_options(&home_dir, "test_gear", Some(&rendered_db), &local_config);

        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), DbOptions::Manager(_)));
    }

    #[test]
    fn test_rendered_db_config_local_global_merges_with_master() {
        // Local global database config merges with master's global config
        let home_dir = std::env::temp_dir().join("toolkit_test_global_merge");
        _ = std::fs::create_dir_all(&home_dir);

        let rendered_db = RenderedDbConfig::new(
            Some(create_global_db_config()),
            Some(create_gear_db_config()),
        );

        let mut local_config = minimal_app_config();
        // Local adds a new server to global database config
        let mut new_servers = HashMap::new();
        new_servers.insert(
            "new_server".to_owned(),
            DbConnConfig {
                engine: Some(toolkit_db::config::DbEngineCfg::Sqlite),
                server: None,
                dsn: Some(toolkit_utils::SecretString::new("sqlite://new.db")),
                host: None,
                port: None,
                user: None,
                password: None,
                dbname: None,
                file: None,
                path: None,
                params: None,
                pool: None,
                lock_keepalive: None,
            },
        );
        local_config.database = Some(GlobalDatabaseConfig {
            servers: new_servers,
            auto_provision: None,
        });

        let result =
            build_merged_db_options(&home_dir, "test_gear", Some(&rendered_db), &local_config);

        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), DbOptions::Manager(_)));
    }
}

// =============================================================================
// Full OoP Config Build Tests
// =============================================================================

mod full_oop_config {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_build_oop_config_standalone_mode() {
        // No rendered config - standalone mode
        let mut local_config = minimal_app_config();
        local_config.logging = [(
            "default".to_owned(),
            logging_section(Some(Level::DEBUG), "logs/standalone.log"),
        )]
        .into();
        local_config.gears.insert(
            "test_gear".to_owned(),
            json!({
                "config": {
                    "setting": "local_value"
                }
            }),
        );

        let result = build_oop_config_and_db(&local_config, "test_gear", None);

        assert!(result.is_ok());
        let (final_config, merged_logging, db_options) = result.unwrap();

        // Config should be from local
        let gear_config = final_config.gears.get("test_gear").unwrap();
        assert_eq!(gear_config["config"]["setting"], "local_value");

        // Logging should be from local
        assert_eq!(merged_logging.len(), 1);
        assert_eq!(
            merged_logging.get("default").unwrap().console_level,
            Some(Level::DEBUG)
        );

        // No database
        assert!(matches!(db_options, DbOptions::None));
    }

    #[test]
    fn test_build_oop_config_with_rendered_config() {
        // With rendered config from master
        let local_config = minimal_app_config();

        let rendered = RenderedGearConfig {
            database: None,
            config: json!({"master_setting": "value"}),
            clients: serde_json::Value::Null,
            logging: Some(
                [(
                    "default".to_owned(),
                    logging_section(Some(Level::INFO), "logs/master.log"),
                )]
                .into(),
            ),
            opentelemetry: None,
        };

        let result = build_oop_config_and_db(&local_config, "test_gear", Some(&rendered));

        assert!(result.is_ok());
        let (final_config, merged_logging, _) = result.unwrap();

        // Config should be from master (local has no config section)
        let gear_config = final_config.gears.get("test_gear").unwrap();
        assert_eq!(gear_config["config"]["master_setting"], "value");

        // Logging from master
        assert_eq!(
            merged_logging.get("default").unwrap().console_level,
            Some(Level::INFO)
        );
    }

    #[test]
    fn test_build_oop_config_local_overrides_master_config() {
        // Local config section completely replaces master
        let mut local_config = minimal_app_config();
        local_config.gears.insert(
            "test_gear".to_owned(),
            json!({
                "config": {
                    "local_setting": "local_value"
                }
            }),
        );

        let rendered = RenderedGearConfig {
            database: None,
            config: json!({
                "master_setting": "master_value",
                "another": "setting"
            }),
            clients: serde_json::Value::Null,
            logging: None,
            opentelemetry: None,
        };

        let result = build_oop_config_and_db(&local_config, "test_gear", Some(&rendered));

        assert!(result.is_ok());
        let (final_config, _, _) = result.unwrap();

        // Config should be from LOCAL (full replacement)
        let gear_config = final_config.gears.get("test_gear").unwrap();
        assert_eq!(gear_config["config"]["local_setting"], "local_value");
        // Master's settings should NOT be present
        assert!(gear_config["config"].get("master_setting").is_none());
    }

    #[test]
    fn rendered_clients_reach_the_child_unless_it_sets_its_own() {
        let rendered = RenderedGearConfig {
            database: None,
            config: json!({}),
            clients: json!({ "consumes": { "billing": { "timeout": "2s" } } }),
            logging: None,
            opentelemetry: None,
        };

        let (from_master, _, _) =
            build_oop_config_and_db(&minimal_app_config(), "test_gear", Some(&rendered)).unwrap();
        assert_eq!(
            from_master.gears["test_gear"]["clients"]["consumes"]["billing"]["timeout"],
            "2s"
        );

        let mut local_config = minimal_app_config();
        local_config.gears.insert(
            "test_gear".to_owned(),
            json!({ "clients": { "consumes": { "billing": { "timeout": "9s" } } } }),
        );
        let (local, _, _) =
            build_oop_config_and_db(&local_config, "test_gear", Some(&rendered)).unwrap();
        assert_eq!(
            local.gears["test_gear"]["clients"]["consumes"]["billing"]["timeout"],
            "9s"
        );
    }

    #[test]
    fn test_build_oop_config_logging_merge() {
        // Logging should merge (key-by-key)
        let mut local_config = minimal_app_config();
        local_config.logging = [
            (
                "default".to_owned(),
                logging_section(Some(Level::DEBUG), "logs/local-default.log"),
            ),
            (
                "new_key".to_owned(),
                logging_section(Some(Level::TRACE), "logs/new.log"),
            ),
        ]
        .into();

        let rendered = RenderedGearConfig {
            database: None,
            config: json!({}),
            clients: serde_json::Value::Null,
            logging: Some(
                [
                    (
                        "default".to_owned(),
                        logging_section(Some(Level::INFO), "logs/master-default.log"),
                    ),
                    (
                        "sqlx".to_owned(),
                        logging_section(Some(Level::WARN), "logs/sql.log"),
                    ),
                ]
                .into(),
            ),
            opentelemetry: None,
        };

        let result = build_oop_config_and_db(&local_config, "test_gear", Some(&rendered));

        assert!(result.is_ok());
        let (_, merged_logging, _) = result.unwrap();

        // 3 keys total: default (overridden), sqlx (from master), new_key (from local)
        assert_eq!(merged_logging.len(), 3);

        // default: overridden by local
        assert_eq!(
            merged_logging.get("default").unwrap().console_level,
            Some(Level::DEBUG)
        );
        assert_eq!(
            merged_logging.get("default").unwrap().file().unwrap(),
            "logs/local-default.log"
        );

        // sqlx: from master
        assert_eq!(
            merged_logging.get("sqlx").unwrap().console_level,
            Some(Level::WARN)
        );

        // new_key: from local
        assert_eq!(
            merged_logging.get("new_key").unwrap().console_level,
            Some(Level::TRACE)
        );
    }

    #[test]
    fn test_build_oop_config_empty_local_config_section() {
        // When local has empty config section (null), use master's
        let mut local_config = minimal_app_config();
        local_config.gears.insert(
            "test_gear".to_owned(),
            json!({
                "config": null
            }),
        );

        let rendered = RenderedGearConfig {
            database: None,
            config: json!({"master_setting": "value"}),
            clients: serde_json::Value::Null,
            logging: None,
            opentelemetry: None,
        };

        let result = build_oop_config_and_db(&local_config, "test_gear", Some(&rendered));

        assert!(result.is_ok());
        let (final_config, _, _) = result.unwrap();

        // Config should be from master since local is null
        let gear_config = final_config.gears.get("test_gear").unwrap();
        assert_eq!(gear_config["config"]["master_setting"], "value");
    }

    #[test]
    fn test_build_oop_config_no_config_section_in_local() {
        // When local has no config section at all, use master's
        let mut local_config = minimal_app_config();
        local_config.gears.insert(
            "test_gear".to_owned(),
            json!({
                "database": {}  // has database but no config
            }),
        );

        let rendered = RenderedGearConfig {
            database: None,
            config: json!({"master_setting": "value"}),
            clients: serde_json::Value::Null,
            logging: None,
            opentelemetry: None,
        };

        let result = build_oop_config_and_db(&local_config, "test_gear", Some(&rendered));

        assert!(result.is_ok());
        let (final_config, _, _) = result.unwrap();

        // Config should be from master
        let gear_config = final_config.gears.get("test_gear").unwrap();
        assert_eq!(gear_config["config"]["master_setting"], "value");
    }

    #[test]
    fn config_gear_name_defaults_to_the_registration_name() {
        let opts = OopRunOptions {
            gear_name: "event-broker".to_owned(),
            ..Default::default()
        };
        assert_eq!(opts.config_gear_name(), "event-broker");
    }

    #[test]
    fn a_role_files_the_rendered_config_under_its_config_name() {
        // A role registers under its own directory name but reads the gear's
        // section: the master's blob must land where `GearCtx::config()` looks.
        let opts = OopRunOptions {
            gear_name: "event-broker-ingest".to_owned(),
            config_gear_name: Some("event-broker".to_owned()),
            ..Default::default()
        };
        assert_eq!(opts.config_gear_name(), "event-broker");

        let rendered = RenderedGearConfig {
            database: None,
            config: json!({"master_setting": "value"}),
            clients: serde_json::Value::Null,
            logging: None,
            opentelemetry: None,
        };
        let (final_config, _, _) = build_oop_config_and_db(
            &minimal_app_config(),
            opts.config_gear_name(),
            Some(&rendered),
        )
        .unwrap();

        assert_eq!(
            final_config.gears["event-broker"]["config"]["master_setting"],
            "value"
        );
        assert!(!final_config.gears.contains_key("event-broker-ingest"));
    }
}

// =============================================================================
// advertise_uri startup validation
// =============================================================================

mod advertise_uri {
    use super::*;

    #[test]
    fn accepts_well_formed_routable_hosts() {
        validate_advertise_uri("http://billing.default.svc.cluster.local:8080", false).unwrap();
        validate_advertise_uri("https://billing:8080", false).unwrap();
        // A routable IP passes without the loopback opt-out.
        validate_advertise_uri("http://10.0.0.5:8080", false).unwrap();
    }

    #[test]
    fn rejects_malformed() {
        // Missing scheme (parses without a host).
        assert!(validate_advertise_uri("billing:8080", false).is_err());
        // Unsupported scheme.
        assert!(validate_advertise_uri("ftp://billing:8080", false).is_err());
        // Not a URL at all.
        assert!(validate_advertise_uri("not a uri", false).is_err());
        // Embedded userinfo (credential smuggling).
        assert!(validate_advertise_uri("http://billing@evil.com:8080", false).is_err());
        assert!(validate_advertise_uri("http://user:pass@billing:8080", false).is_err());
        // Missing-host authority.
        assert!(validate_advertise_uri("http://", false).is_err());
    }

    #[test]
    fn rejects_loopback_and_unspecified_by_default() {
        // ADR-0009 section 5: a loopback / unspecified advertise_uri is a
        // registered-but-unreachable instance in multi-host Profile 2 / 3.
        for uri in [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            // Fully-qualified `localhost.` (trailing dot) is DNS-equivalent to
            // `localhost`, so it must still be treated as loopback.
            "http://localhost.:8080",
            "http://0.0.0.0:8080",
            "http://[::1]:8080",
            "http://[::]:8080",
        ] {
            assert!(
                validate_advertise_uri(uri, false).is_err(),
                "{uri} must be rejected without the loopback opt-out"
            );
        }
        // The generated default (unspecified bind -> loopback) is exactly the
        // unset-advertise_uri trap and must be rejected too.
        let default = default_advertise_uri("0.0.0.0:8080".parse().unwrap());
        assert!(validate_advertise_uri(&default, false).is_err());
    }

    #[test]
    fn allows_loopback_when_opted_in() {
        // Single-host / local-dev opt-out (oop_http.allow_loopback_advertise).
        for uri in [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "http://localhost.:8080",
            "http://0.0.0.0:8080",
            "http://[::1]:8080",
        ] {
            validate_advertise_uri(uri, true).unwrap();
        }
        // The default still passes its own validation with the opt-out.
        let default = default_advertise_uri("0.0.0.0:8080".parse().unwrap());
        validate_advertise_uri(&default, true).unwrap();
    }
}

/// The `OoP`-specific platform-credential rules. Building the credential itself
/// is tested in `toolkit_contract::runtime::platform_credential`.
mod platform_credentials {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio_util::sync::CancellationToken;
    use toolkit_security::{InternalAuthConfig, KubePresent, KubeVerify};

    fn unique_dir(tag: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("oop-cred-{tag}-{}-{n}", std::process::id()))
    }

    #[tokio::test]
    async fn kube_without_present_block_is_a_startup_error() {
        // An OoP gear always registers and heartbeats with DirectoryService, so
        // it can never be inbound-only: no `present` block is a config error,
        // not "attach nothing".
        let cfg = InternalAuthConfig::Kube {
            verify: Some(KubeVerify {
                audiences: vec!["toolkit-internal".to_owned()],
            }),
            present: None,
        };
        let Err(err) =
            build_directory_client("http://127.0.0.1:1", Some(&cfg), &CancellationToken::new())
                .await
        else {
            panic!("kube without a present block must not build the directory client");
        };
        assert!(err.to_string().contains("present"), "got: {err}");
    }

    #[tokio::test]
    async fn kube_without_verify_block_refuses_the_inbound_validator() {
        // The OoP gear validates inbound platform calls on its own routes, so a
        // kube config without `verify` must fail startup rather than leave the
        // inbound side unauthenticated.
        let cfg = InternalAuthConfig::Kube {
            verify: None,
            present: Some(KubePresent {
                token_path: unique_dir("kube-noverify").join("token"),
            }),
        };
        let err = build_internal_authenticator(Some(&cfg))
            .await
            .expect_err("no verify block means no inbound validator");
        assert!(err.to_string().contains("verify"), "got: {err}");
    }
}

/// Tests for `build_directory_client`: the bootstrap slice that must build a
/// `DirectoryService` client even when the directory is unreachable
/// (`cpt-cf-adr-eventual-readiness`).
mod directory_client_bootstrap {
    use super::*;
    use cf_system_sdks::directory::{DIRECTORY_SERVICE_NAME, DirectoryClient};
    use tokio_util::sync::CancellationToken;
    use toolkit_security::InternalAuthConfig;

    #[tokio::test]
    async fn builds_against_unreachable_directory_without_credential() {
        // Nothing is listening on port 1, yet the client builds: bootstrap is
        // not blocked on directory reachability.
        let (client, provider) =
            build_directory_client("http://127.0.0.1:1", None, &CancellationToken::new())
                .await
                .expect("lazy directory client must build against an unreachable directory");
        assert!(
            provider.is_none(),
            "no internal_auth => no outbound credential"
        );

        // The deferred connect surfaces as an RPC error, not a hang.
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            client.resolve_grpc_service(DIRECTORY_SERVICE_NAME),
        )
        .await;
        assert!(outcome.is_ok(), "first RPC must not hang");
        assert!(
            outcome.unwrap().is_err(),
            "first RPC to an unreachable directory must return Err"
        );
    }

    #[tokio::test]
    async fn builds_against_unreachable_directory_with_credential() {
        // Same guarantee on the credentialed path: the client still builds and
        // a configured credential yields an outbound provider.
        let cfg = InternalAuthConfig::SharedSecret {
            secret: secrecy::SecretString::from("shared-tok"),
            peer_name: "toolkit-internal".to_owned(),
        };
        let (_client, provider) =
            build_directory_client("http://127.0.0.1:1", Some(&cfg), &CancellationToken::new())
                .await
                .expect("credentialed lazy client must build against an unreachable directory");
        assert!(
            provider.is_some(),
            "shared secret must yield an outbound token provider"
        );
    }

    #[tokio::test]
    async fn rejects_malformed_endpoint() {
        // A malformed endpoint must still fail fast.
        assert!(
            build_directory_client("", None, &CancellationToken::new())
                .await
                .is_err(),
            "a malformed directory endpoint must fail fast"
        );
    }

    #[tokio::test]
    async fn malformed_endpoint_is_reported_before_credential_failure() {
        // Both inputs are broken: a malformed endpoint AND a kube token_path
        // that does not exist. The endpoint is validated first, so the error is
        // the endpoint error — never the credential one (which would otherwise
        // mask the misconfigured endpoint).
        let cfg = InternalAuthConfig::Kube {
            verify: Some(toolkit_security::KubeVerify {
                audiences: vec!["toolkit-internal".to_owned()],
            }),
            present: Some(toolkit_security::KubePresent {
                token_path: std::env::temp_dir()
                    .join("cf-oop-missing-token-regression")
                    .join("token"),
            }),
        };
        let Err(err) = build_directory_client("", Some(&cfg), &CancellationToken::new()).await
        else {
            panic!("a malformed endpoint must fail");
        };
        let msg = format!("{err:#}");
        assert!(
            !msg.contains("service-account token"),
            "endpoint must be validated before credential work; got credential error: {msg}"
        );
    }
}

/// `oop_http.auth_disabled` reaches the serve options: off when omitted, as written
/// otherwise.
mod auth_disabled {
    use super::*;
    use crate::bootstrap::config::OopHttpConfig;
    use crate::directory::test_support::ManagerDirectoryClient;
    use crate::runtime::GearManager;

    async fn serve_auth_disabled(extra: serde_json::Value) -> bool {
        let mut raw = serde_json::json!({
            "listen_addr": "127.0.0.1:0",
            "allow_loopback_advertise": true,
        });
        if let (Some(raw), Some(extra)) = (raw.as_object_mut(), extra.as_object()) {
            raw.extend(extra.clone());
        }
        let cfg: OopHttpConfig = serde_json::from_value(raw).expect("valid oop_http");
        let directory: Arc<dyn DirectoryClient> =
            Arc::new(ManagerDirectoryClient::new(Arc::new(GearManager::new())));
        build_oop_serve_options(
            &cfg,
            "gear",
            Uuid::new_v4(),
            None,
            Duration::from_secs(5),
            directory,
        )
        .await
        .expect("options build")
        .auth_disabled
    }

    #[tokio::test]
    async fn is_read_from_config() {
        assert!(!serve_auth_disabled(serde_json::json!({})).await, "omitted");
        assert!(
            !serve_auth_disabled(serde_json::json!({ "auth_disabled": false })).await,
            "false"
        );
        assert!(
            serve_auth_disabled(serde_json::json!({ "auth_disabled": true })).await,
            "true"
        );
    }
}

/// The deployment publication job and the `OoP` runtime build the same GTS
/// manifest from the same local file and rendered master configuration, and
/// the job opens no database.
mod publication_config_tests {
    use crate::bootstrap::config::AppConfig;
    use crate::bootstrap::oop::{
        OopRunOptions, build_oop_config_and_db, oop_publication_config_from, parse_rendered_config,
    };
    use crate::gts_publication::{
        DeclarationSource, GtsPublicationBackend, GtsPublicationDescriptor, GtsPublicationInputs,
    };
    use serde_json::json;

    fn no_declarations() -> Vec<DeclarationSource> {
        Vec::new()
    }

    /// The gear's whole effective config section, as one document.
    fn config_document(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(gts::GtsId, serde_json::Value)>> {
        let cfg: serde_json::Value = inputs.config_or_default()?;
        Ok(vec![(
            gts::GtsId::try_new("gts.cf.test.publication.config.v1~")?,
            cfg,
        )])
    }

    fn unused_backend(_: &crate::ClientHub) -> anyhow::Result<GtsPublicationBackend> {
        anyhow::bail!("not used")
    }

    #[allow(
        clippy::unnecessary_wraps,
        clippy::needless_pass_by_value,
        reason = "must match the WireFn signature the gear macro emits"
    )]
    fn unused_wire(
        _: &crate::ClientHub,
        _: std::sync::Arc<dyn crate::discovery::EndpointResolver>,
        _: toolkit_contract::wiring::ClientTuning,
    ) -> anyhow::Result<crate::discovery::WireOutcome> {
        anyhow::bail!("not used")
    }

    static DESCRIPTOR: GtsPublicationDescriptor = GtsPublicationDescriptor {
        gear: "test_gear",
        publisher_version: "1.0.0",
        declarations: no_declarations,
        entities: Some(config_document),
        backend: unused_backend,
        provider: "registry",
        wire: unused_wire,
    };

    fn db_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return found;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(db_files(&path));
            } else if path.extension().is_some_and(|e| e == "db") {
                found.push(path);
            }
        }
        found
    }

    fn check(local_gear_section: &str, expected: &serde_json::Value) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let config_path = dir.path().join("unit.yaml");
        // Forward slashes: a Windows `C:\Users\...` inside a double-quoted YAML
        // scalar is read as escape sequences (`\U` is a hex escape).
        let home_yaml = home.to_string_lossy().replace('\\', "/");
        std::fs::write(
            &config_path,
            format!(
                "server:\n  home_dir: \"{home_yaml}\"\ngears:\n  test_gear:\n{local_gear_section}"
            ),
        )
        .unwrap();
        let rendered = json!({
            "config": { "vendor": "master", "priority": 3 },
            "database": {
                "global": { "servers": { "sqlite": { "engine": "sqlite" } } },
                "gear": { "server": "sqlite", "file": "unit.db" },
            },
        })
        .to_string();
        let opts = OopRunOptions {
            gear_name: "test_gear".to_owned(),
            config_path: Some(config_path.clone()),
            ..Default::default()
        };

        let rendered = parse_rendered_config(Some(&rendered)).unwrap();
        let deploy = oop_publication_config_from(&opts, rendered.as_ref()).unwrap();
        assert!(db_files(dir.path()).is_empty(), "the job opens no database");

        let mut local = AppConfig::load_or_default(Some(&config_path)).unwrap();
        local.apply_cli_overrides(0);
        let rendered = rendered.unwrap();
        let (runtime, _, _) =
            build_oop_config_and_db(&local, "test_gear", Some(&rendered)).unwrap();

        let deploy = DESCRIPTOR.build_gts_manifest(&deploy).unwrap();
        let runtime = DESCRIPTOR.build_gts_manifest(&runtime).unwrap();
        assert_eq!(deploy, runtime);
        assert_eq!(&deploy.documents()[0].1, expected);
    }

    // `AppConfig::load_or_default` reads the process `APP__` environment, which
    // the `#[serial]` config tests set.
    #[test]
    #[serial_test::serial]
    fn the_master_config_reaches_the_manifest_when_the_unit_has_none() {
        check("    {}\n", &json!({ "vendor": "master", "priority": 3 }));
    }

    // `AppConfig::load_or_default` reads the process `APP__` environment, which
    // the `#[serial]` config tests set.
    #[test]
    #[serial_test::serial]
    fn a_local_config_section_overrides_the_master_one_in_both_paths() {
        check(
            "    config:\n      vendor: \"local\"\n",
            &json!({ "vendor": "local" }),
        );
    }

    /// An absent variable is standalone mode; a present but invalid one fails,
    /// for the runtime and the deployment job alike (both read it through
    /// `rendered_config_from_env`).
    #[test]
    fn a_present_but_invalid_rendered_config_fails() {
        assert!(parse_rendered_config(None).unwrap().is_none());
        let err = parse_rendered_config(Some("{not json")).unwrap_err();
        assert!(
            format!("{err:#}").contains("TOOLKIT_MODULE_CONFIG is set but invalid"),
            "{err:#}"
        );
    }
}

mod composition_finalization {
    use super::*;
    use futures_util::FutureExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::advance;

    #[derive(Debug, thiserror::Error)]
    #[error("fixture bootstrap failure")]
    struct BootstrapFailure;

    #[tokio::test]
    async fn finalization_preserves_each_runtime_and_cleanup_result() {
        for (host_fails, cleanup_fails) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let cleanup = ClientCleanup::new(
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    if cleanup_fails {
                        Err(ClientCleanupError::WorkerFailed)
                    } else {
                        Ok(())
                    }
                }),
                CancellationToken::new(),
            );
            let host = async {
                if host_fails {
                    Err(BootstrapFailure.into())
                } else {
                    Ok(())
                }
            };
            let result = finalize_composition(
                host,
                cleanup,
                &CancellationToken::new(),
                &ServerConfig::default(),
            )
            .await;
            match (host_fails, cleanup_fails, result) {
                (false, false, Ok(()))
                | (
                    false,
                    true,
                    Err(OopCompositionRunError::Cleanup(ClientCleanupError::WorkerFailed)),
                ) => {}
                (true, false, Err(OopCompositionRunError::Bootstrap { source }))
                | (
                    true,
                    true,
                    Err(OopCompositionRunError::BootstrapAndCleanup {
                        source,
                        cleanup: ClientCleanupError::WorkerFailed,
                    }),
                ) => {
                    assert!(source.downcast_ref::<BootstrapFailure>().is_some());
                    assert_eq!(source.to_string(), "fixture bootstrap failure");
                }
                other => panic!("wrong result pair: {other:?}"),
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    struct HeldCleanup {
        entered: Arc<AtomicUsize>,
        completed: Arc<AtomicUsize>,
        release: CancellationToken,
        abort: CancellationToken,
    }

    impl HeldCleanup {
        fn new() -> Self {
            Self {
                entered: Arc::default(),
                completed: Arc::default(),
                release: CancellationToken::new(),
                abort: CancellationToken::new(),
            }
        }
        fn owner(&self) -> ClientCleanup {
            let entered = self.entered.clone();
            let completed = self.completed.clone();
            let release = self.release.clone();
            ClientCleanup::new(
                Box::pin(async move {
                    entered.fetch_add(1, Ordering::SeqCst);
                    release.cancelled().await;
                    completed.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }),
                self.abort.clone(),
            )
        }
        fn state(&self) -> (usize, usize, bool) {
            (
                self.entered.load(Ordering::SeqCst),
                self.completed.load(Ordering::SeqCst),
                self.abort.is_cancelled(),
            )
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_uses_the_configured_shutdown_deadline() {
        for host_failed in [false, true] {
            let held = HeldCleanup::new();
            let cancel = CancellationToken::new();
            let server = ServerConfig {
                shutdown_deadline_secs: Some(2),
                ..Default::default()
            };
            let host = async {
                if host_failed {
                    Err(BootstrapFailure.into())
                } else {
                    Ok(())
                }
            };
            let mut run = Box::pin(finalize_composition(host, held.owner(), &cancel, &server));
            assert!(run.as_mut().now_or_never().is_none());
            advance(Duration::from_millis(1999)).await;
            assert!(run.as_mut().now_or_never().is_none());
            assert_eq!(held.state(), (1, 0, false));
            advance(Duration::from_millis(1)).await;
            match (host_failed, run.now_or_never()) {
                (
                    false,
                    Some(Err(OopCompositionRunError::Cleanup(
                        ClientCleanupError::DeadlineExceeded,
                    ))),
                ) => {}
                (
                    true,
                    Some(Err(OopCompositionRunError::BootstrapAndCleanup {
                        source,
                        cleanup: ClientCleanupError::DeadlineExceeded,
                    })),
                ) => {
                    assert!(source.downcast_ref::<BootstrapFailure>().is_some());
                    assert_eq!(source.to_string(), "fixture bootstrap failure");
                }
                other => panic!("expected configured deadline: {other:?}"),
            }
            assert_eq!(held.state(), (1, 0, true));
        }
        let held = HeldCleanup::new();
        let server = ServerConfig {
            shutdown_deadline_secs: Some(0),
            ..Default::default()
        };
        let result = tokio::time::timeout(
            Duration::from_millis(1),
            finalize_composition(
                async { Ok(()) },
                held.owner(),
                &CancellationToken::new(),
                &server,
            ),
        )
        .await
        .unwrap();
        assert!(matches!(
            result,
            Err(OopCompositionRunError::Cleanup(
                ClientCleanupError::DeadlineExceeded
            ))
        ));
        assert_eq!(held.state(), (1, 0, true));
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_uses_the_toolkit_default_shutdown_deadline() {
        assert_eq!(
            crate::runtime::DEFAULT_SHUTDOWN_DEADLINE,
            Duration::from_secs(35)
        );
        let held = HeldCleanup::new();
        let cancel = CancellationToken::new();
        let server = ServerConfig::default();
        let mut run = Box::pin(finalize_composition(
            async { Ok(()) },
            held.owner(),
            &cancel,
            &server,
        ));
        assert!(run.as_mut().now_or_never().is_none());
        advance(Duration::from_millis(34999)).await;
        assert!(run.as_mut().now_or_never().is_none());
        assert_eq!(held.state(), (1, 0, false));
        advance(Duration::from_millis(1)).await;
        assert!(matches!(
            run.now_or_never(),
            Some(Err(OopCompositionRunError::Cleanup(
                ClientCleanupError::DeadlineExceeded
            )))
        ));
        assert_eq!(held.state(), (1, 0, true));
    }

    #[tokio::test(start_paused = true)]
    async fn completed_cleanup_before_the_deadline_is_not_aborted() {
        let held = HeldCleanup::new();
        let cancel = CancellationToken::new();
        let server = ServerConfig {
            shutdown_deadline_secs: Some(2),
            ..Default::default()
        };
        let mut run = Box::pin(finalize_composition(
            async { Ok(()) },
            held.owner(),
            &cancel,
            &server,
        ));
        assert!(run.as_mut().now_or_never().is_none());
        advance(Duration::from_secs(1)).await;
        held.release.cancel();
        assert!(matches!(run.now_or_never(), Some(Ok(()))));
        advance(Duration::from_secs(2)).await;
        assert_eq!(held.state(), (1, 1, false));
    }

    #[tokio::test]
    async fn dropping_pending_preparation_aborts_owned_clients() {
        let held = HeldCleanup::new();
        let cancel = CancellationToken::new();
        let server = ServerConfig::default();
        let mut run = Box::pin(finalize_composition(
            std::future::pending(),
            held.owner(),
            &cancel,
            &server,
        ));
        assert!(run.as_mut().now_or_never().is_none());
        assert_eq!(held.state(), (0, 0, false));
        drop(run);
        assert_eq!(held.state(), (0, 0, true));
    }

    #[tokio::test]
    async fn the_runner_receives_the_bootstrap_directory_client() {
        let ready = Prepared {
            config: Arc::new(minimal_app_config()),
            db: DbOptions::None,
            cancel: CancellationToken::new(),
            instance_id: Uuid::from_u128(0x31),
        };
        let connected = ready.connect("http://127.0.0.1:9").await.unwrap();
        let [directory] = connected
            .run_options
            .clients
            .try_into()
            .unwrap_or_else(|_| panic!("one directory registration"));
        let hub = crate::client_hub::ClientHub::default();
        directory.apply(&hub);
        assert!(Arc::ptr_eq(
            &hub.get::<dyn DirectoryClient>().unwrap(),
            &connected.directory
        ));
    }
}

mod signal_readiness {
    use super::*;
    use parking_lot::Mutex;
    use std::{
        future::Future,
        io::{self, Write},
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll},
    };
    use tokio::sync::oneshot;
    use tokio::time::timeout;

    const BOUND: Duration = Duration::from_secs(10);

    #[derive(Default)]
    struct Observations {
        polls: AtomicUsize,
        drops: AtomicUsize,
        dropped: CancellationToken,
    }

    struct Waiter<T> {
        observations: Arc<Observations>,
        gate: oneshot::Receiver<T>,
    }

    impl<T> Waiter<T> {
        fn new(observations: &Arc<Observations>) -> (Self, oneshot::Sender<T>) {
            let (release, gate) = oneshot::channel();
            (
                Self {
                    observations: observations.clone(),
                    gate,
                },
                release,
            )
        }
    }

    impl<T> Future for Waiter<T> {
        type Output = T;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
            assert!(!self.gate.is_terminated(), "waiter polled after completion");
            self.observations.polls.fetch_add(1, Ordering::SeqCst);
            Pin::new(&mut self.gate)
                .poll(cx)
                .map(|result| result.expect("waiter gate"))
        }
    }

    impl<T> Drop for Waiter<T> {
        fn drop(&mut self) {
            self.observations.drops.fetch_add(1, Ordering::SeqCst);
            self.observations.dropped.cancel();
        }
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    struct DiagnosticObservation {
        polls_at_first_write: Option<usize>,
        bytes: Vec<u8>,
    }

    struct Diagnostic {
        fallback: Arc<Observations>,
        observation: Arc<Mutex<DiagnosticObservation>>,
        fail: bool,
    }

    impl Diagnostic {
        fn new(fallback: &Arc<Observations>) -> (Self, Arc<Mutex<DiagnosticObservation>>) {
            let observation = Arc::new(Mutex::new(DiagnosticObservation::default()));
            (
                Self {
                    fallback: fallback.clone(),
                    observation: observation.clone(),
                    fail: false,
                },
                observation,
            )
        }
    }

    impl Write for Diagnostic {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut observation = self.observation.lock();
            observation
                .polls_at_first_write
                .get_or_insert_with(|| self.fallback.polls.load(Ordering::SeqCst));
            if self.fail {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            observation.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_immediate_shutdown_cancels_before_preparation() {
        let seen = Arc::new(Observations::default());
        let (wait, release) = Waiter::new(&seen);
        release.send(()).unwrap();
        let process = arm_shutdown_wait(wait);
        assert_eq!(
            (
                seen.polls.load(Ordering::SeqCst),
                process.is_cancelled(),
                seen.drops.load(Ordering::SeqCst)
            ),
            (1, true, 1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_primed_waiter_survives_until_shutdown() {
        let seen = Arc::new(Observations::default());
        let (wait, release) = Waiter::new(&seen);
        let process = arm_shutdown_wait(wait);
        assert_eq!(
            (
                seen.polls.load(Ordering::SeqCst),
                process.is_cancelled(),
                seen.drops.load(Ordering::SeqCst)
            ),
            (1, false, 0)
        );

        release.send(()).unwrap();
        timeout(BOUND, process.cancelled())
            .await
            .expect("shutdown signal cancels process");
        assert_eq!(seen.drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_process_releases_its_pending_waiter() {
        let seen = Arc::new(Observations::default());
        let (wait, _release) = Waiter::new(&seen);
        let process = arm_shutdown_wait(wait);
        assert_eq!(
            (
                seen.polls.load(Ordering::SeqCst),
                process.is_cancelled(),
                seen.drops.load(Ordering::SeqCst)
            ),
            (1, false, 0)
        );

        process.cancel();
        timeout(BOUND, seen.dropped.cancelled())
            .await
            .expect("process cancellation drops waiter");
        assert_eq!(seen.drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_registration_failure_is_reported_after_the_fallback_is_armed() {
        let seen = Arc::new(Observations::default());
        let (fallback, release) = Waiter::new(&seen);
        let (diagnostic, captured) = Diagnostic::new(&seen);
        let process = arm_shutdown_wait(wait_for_shutdown_or_fallback(
            std::future::ready(Err(anyhow::anyhow!("fixture primary registration failed"))),
            fallback,
            diagnostic,
        ));

        assert_eq!(
            *captured.lock(),
            DiagnosticObservation {
                polls_at_first_write: Some(1),
                bytes: b"OoP signal waiter failed, falling back to ctrl_c(): \
                         fixture primary registration failed\n"
                    .to_vec(),
            }
        );
        assert_eq!(
            (process.is_cancelled(), seen.drops.load(Ordering::SeqCst)),
            (false, 0)
        );

        release.send(Ok(())).unwrap();
        timeout(BOUND, process.cancelled())
            .await
            .expect("fallback signal cancels process");
        assert_eq!(
            (process.is_cancelled(), seen.drops.load(Ordering::SeqCst)),
            (true, 1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_ready_fallback_is_not_polled_twice() {
        for outcome in [Ok(()), Err(io::Error::other("fixture fallback failed"))] {
            let seen = Arc::new(Observations::default());
            let (fallback, release) = Waiter::new(&seen);
            release.send(outcome).unwrap();
            let (diagnostic, captured) = Diagnostic::new(&seen);
            let process = arm_shutdown_wait(wait_for_shutdown_or_fallback(
                std::future::ready(Err(anyhow::anyhow!("fixture primary registration failed"))),
                fallback,
                diagnostic,
            ));

            assert_eq!(
                (
                    seen.polls.load(Ordering::SeqCst),
                    process.is_cancelled(),
                    seen.drops.load(Ordering::SeqCst)
                ),
                (1, true, 1)
            );
            assert_eq!(
                *captured.lock(),
                DiagnosticObservation {
                    polls_at_first_write: Some(1),
                    bytes: b"OoP signal waiter failed, falling back to ctrl_c(): \
                             fixture primary registration failed\n"
                        .to_vec(),
                }
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_diagnostic_write_failure_does_not_end_the_fallback_wait() {
        let seen = Arc::new(Observations::default());
        let (fallback, release) = Waiter::new(&seen);
        let (mut diagnostic, captured) = Diagnostic::new(&seen);
        diagnostic.fail = true;
        let process = arm_shutdown_wait(wait_for_shutdown_or_fallback(
            std::future::ready(Err(anyhow::anyhow!("fixture primary registration failed"))),
            fallback,
            diagnostic,
        ));

        assert_eq!(
            *captured.lock(),
            DiagnosticObservation {
                polls_at_first_write: Some(1),
                bytes: vec![],
            }
        );
        assert_eq!(
            (process.is_cancelled(), seen.drops.load(Ordering::SeqCst)),
            (false, 0)
        );

        release.send(Ok(())).unwrap();
        timeout(BOUND, process.cancelled())
            .await
            .expect("fallback signal cancels process despite a diagnostic I/O error");
        assert_eq!(
            (process.is_cancelled(), seen.drops.load(Ordering::SeqCst)),
            (true, 1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_primary_signal_does_not_poll_the_fallback() {
        let primary_seen = Arc::new(Observations::default());
        let (primary, release) = Waiter::new(&primary_seen);
        release.send(Ok(())).unwrap();
        let fallback_seen = Arc::new(Observations::default());
        let (fallback, _held) = Waiter::new(&fallback_seen);
        let (diagnostic, captured) = Diagnostic::new(&fallback_seen);
        let process =
            arm_shutdown_wait(wait_for_shutdown_or_fallback(primary, fallback, diagnostic));

        assert_eq!(
            (
                primary_seen.polls.load(Ordering::SeqCst),
                primary_seen.drops.load(Ordering::SeqCst),
                fallback_seen.polls.load(Ordering::SeqCst),
                fallback_seen.drops.load(Ordering::SeqCst),
                process.is_cancelled()
            ),
            (1, 1, 0, 1, true)
        );
        assert_eq!(
            *captured.lock(),
            DiagnosticObservation {
                polls_at_first_write: None,
                bytes: vec![],
            }
        );
    }
}

mod request_limits {
    use super::*;
    use crate::bootstrap::config::OopHttpConfig;
    use crate::directory::test_support::ManagerDirectoryClient;
    use crate::runtime::{GearManager, OopRequestLimits};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const BOUND: Duration = Duration::from_secs(10);
    const INSTANCE: Uuid = uuid::uuid!("00000000-0000-0000-0000-000000000031");
    const BYTES: usize = 16_777_216;
    const TIMEOUT: Duration = Duration::from_secs(30);
    const CHILD_HOME: &str = "TOOLKIT_OOP_REQUEST_LIMITS_TEST_HOME";
    static FACTORY_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn config(raw_limits: Value) -> OopHttpConfig {
        serde_json::from_value(raw_limits).expect("valid OoP HTTP config")
    }

    #[test]
    fn request_limits_round_trip_and_reject_invalid_configuration() {
        let without_limits = json!({"listen_addr":"127.0.0.1:0"});
        let mut with_null = without_limits.clone();
        with_null["request_limits"] = Value::Null;
        for raw in [without_limits.clone(), with_null] {
            let config = config(raw);
            assert_eq!(config.request_limits, None);
            assert_eq!(
                serde_json::to_value(config).expect("serialize config"),
                json!({
                    "listen_addr":"127.0.0.1:0",
                    "probe_bind_addr":null,
                    "drain_timeout_secs":30,
                    "healthcheck_timeout_ms":500,
                    "advertise_uri":null,
                    "allow_loopback_advertise":false,
                    "auth_disabled":false,
                    "internal_auth":null,
                    "labels":{}
                })
            );
        }

        let valid = json!({"body_limit_bytes":BYTES,"request_timeout":"30s"});
        let mut raw = without_limits.clone();
        raw["request_limits"] = valid.clone();
        let parsed = config(raw);
        assert_eq!(
            parsed.request_limits,
            Some(OopRequestLimits::new(BYTES, TIMEOUT).expect("valid policy"))
        );
        assert_eq!(
            serde_json::to_value(parsed.request_limits).expect("serialize policy"),
            valid
        );

        let mut raw = without_limits.clone();
        raw["request_limits"] = json!({"body_limit_bytes":1,"request_timeout":"1ns"});
        assert_eq!(
            config(raw).request_limits,
            Some(OopRequestLimits::new(1, Duration::from_nanos(1)).expect("positive minimum"))
        );

        for (input, expected) in [
            (
                json!({"body_limit_bytes":0,"request_timeout":"30s"}),
                "body_limit_bytes must be positive",
            ),
            (
                json!({"body_limit_bytes":4,"request_timeout":"0s"}),
                "request_timeout must be positive",
            ),
            (
                json!({"request_timeout":"30s"}),
                "missing field `body_limit_bytes`",
            ),
            (
                json!({"body_limit_bytes":4}),
                "missing field `request_timeout`",
            ),
            (
                json!({"body_limit_bytes":4,"request_timeout":30}),
                "invalid type: integer `30`, expected a duration",
            ),
            (
                json!({"body_limit_bytes":-1,"request_timeout":"30s"}),
                "invalid value: integer `-1`, expected usize",
            ),
            (
                json!({"body_limit_bytes":4,"request_timeout":"30s","extra":true}),
                "unknown field `extra`, expected `body_limit_bytes` or `request_timeout`",
            ),
        ] {
            let mut raw = without_limits.clone();
            raw["request_limits"] = input;
            assert_eq!(
                serde_json::from_value::<OopHttpConfig>(raw)
                    .expect_err("invalid policy")
                    .to_string(),
                expected
            );
        }
    }

    fn factory_sentinel(
        _config: &AppConfig,
        _context: &OopCompositionContext,
    ) -> Result<OopComposition, OopCompositionError> {
        FACTORY_CALLS.fetch_add(1, Ordering::SeqCst);
        Err(OopCompositionError::ClientConstruction {
            component: "factory_sentinel",
        })
    }

    async fn check_composition_controls(home: &std::path::Path) {
        let base = json!({
            "server":{"home_dir":home},
            "oop_http":{"listen_addr":"127.0.0.1:0","allow_loopback_advertise":true}
        });
        let mut zero_bytes = base.clone();
        zero_bytes["oop_http"]["request_limits"] =
            json!({"body_limit_bytes":0,"request_timeout":"30s"});
        let mut zero_timeout = base.clone();
        zero_timeout["oop_http"]["request_limits"] =
            json!({"body_limit_bytes":4,"request_timeout":"0s"});
        let mut null = base.clone();
        null["oop_http"]["request_limits"] = Value::Null;
        for (config, reason) in [
            (zero_bytes, Some("body_limit_bytes must be positive")),
            (zero_timeout, Some("request_timeout must be positive")),
            (base, None),
            (null, None),
        ] {
            FACTORY_CALLS.store(0, Ordering::SeqCst);
            let config_path = home.join("config.yaml");
            std::fs::write(
                &config_path,
                serde_saphyr::to_string(&config).expect("fixture YAML"),
            )
            .expect("write config");
            let options = OopRunOptions {
                gear_name: "limits-gear".to_owned(),
                config_gear_name: None,
                instance_id: Some(INSTANCE),
                directory_endpoint: "http://127.0.0.1:9".to_owned(),
                config_path: Some(config_path),
                verbose: 0,
                print_config: false,
                heartbeat_interval_secs: 5,
                version: Some("1.2.3".to_owned()),
            };
            let result =
                tokio::time::timeout(BOUND, run_oop_with_composition(options, factory_sentinel))
                    .await
                    .expect("bootstrap result");
            let factory_calls = FACTORY_CALLS.load(Ordering::SeqCst);
            if let Some(reason) = reason {
                let Err(OopCompositionRunError::Bootstrap { source }) = result else {
                    panic!(
                        "expected bootstrap validation with 0 factory calls, got {factory_calls}: {result:?}"
                    );
                };
                assert_eq!(source.to_string(), "Failed to extract config from figment");
                let detail = source
                    .source()
                    .and_then(|error| error.downcast_ref::<figment::Error>())
                    .expect("Figment config extraction error");
                assert_eq!(
                    (detail.kind.clone(), detail.path.clone(), detail.count()),
                    (
                        figment::error::Kind::Message(reason.to_owned()),
                        vec!["oop_http".to_owned(), "request_limits".to_owned()],
                        1,
                    )
                );
                assert_eq!(factory_calls, 0);
            } else {
                let Err(OopCompositionRunError::Composition(error)) = result else {
                    panic!("expected factory control: {result:?}");
                };
                assert_eq!(
                    error,
                    OopCompositionError::ClientConstruction {
                        component: "factory_sentinel"
                    }
                );
                assert_eq!(factory_calls, 1);
            }
        }
    }

    #[test]
    fn invalid_limits_fail_before_composition_runs() {
        if let Some(home) = std::env::var_os(CHILD_HOME) {
            temp_env::with_var(TOOLKIT_MODULE_CONFIG_ENV, None::<&str>, || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("child runtime")
                    .block_on(check_composition_controls(std::path::Path::new(&home)));
            });
            return;
        }

        // A successful prepare installs the process-wide logger and panic hook.
        let home = tempfile::tempdir().expect("child home");
        let (_, module) = module_path!().split_once("::").expect("test module path");
        let test = format!("{module}::invalid_limits_fail_before_composition_runs");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("parent runtime")
            .block_on(check_bootstrap_child(&test, home.path()));
    }

    async fn check_bootstrap_child(test: &str, home: &std::path::Path) {
        let stdout = tempfile::NamedTempFile::new_in(home).expect("child stdout");
        let stderr = tempfile::NamedTempFile::new_in(home).expect("child stderr");
        // Concurrent config tests can set Figment overrides while this child starts.
        let child_env = std::env::vars_os().filter(|(key, _)| {
            !key.to_string_lossy()
                .trim()
                .to_ascii_uppercase()
                .starts_with("APP__")
        });
        let mut child = tokio::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", test, "--nocapture"])
            .env_clear()
            .envs(child_env)
            .env(CHILD_HOME, home)
            .stdout(stdout.reopen().expect("child stdout file"))
            .stderr(stderr.reopen().expect("child stderr file"))
            .kill_on_drop(true)
            .spawn()
            .expect("spawn bootstrap child");
        let wait = tokio::time::timeout(BOUND, child.wait()).await;
        let cleanup = if matches!(wait, Ok(Ok(_))) {
            None
        } else {
            Some(tokio::time::timeout(BOUND, child.kill()).await)
        };
        let out = std::fs::read_to_string(stdout.path()).expect("read child stdout");
        let err = std::fs::read_to_string(stderr.path()).expect("read child stderr");
        let Ok(Ok(status)) = wait else {
            panic!(
                "bootstrap child wait={wait:?}, kill/reap={cleanup:?}\nstdout={out}\nstderr={err}"
            );
        };
        assert!(
            status.success() && out.contains("test result: ok. 1 passed; 0 failed"),
            "bootstrap child status={status}\nstdout={out}\nstderr={err}",
        );
    }

    #[tokio::test]
    async fn bootstrap_forwards_request_limits_without_changing_other_options() {
        let policy = OopRequestLimits::new(BYTES, TIMEOUT).expect("valid policy");
        let base = json!({
            "listen_addr":"127.0.0.1:8080",
            "probe_bind_addr":"127.0.0.1:8081",
            "drain_timeout_secs":17,
            "healthcheck_timeout_ms":777,
            "advertise_uri":"http://127.0.0.1:8000",
            "allow_loopback_advertise":true
        });
        let mut limited = base.clone();
        limited["request_limits"] = json!({"body_limit_bytes":BYTES,"request_timeout":"30s"});
        let directory: Arc<dyn DirectoryClient> =
            Arc::new(ManagerDirectoryClient::new(Arc::new(GearManager::new())));
        for (raw, expected_limits) in [(base, None), (limited, Some(policy))] {
            let cfg = config(raw);
            let OopServeOptions {
                gear_name,
                instance_id,
                version,
                advertise_uri,
                listen_addr,
                probe_bind_addr,
                drain_timeout,
                heartbeat_interval,
                healthcheck_timeout,
                request_limits,
                directory: actual_directory,
                bearer_authenticator,
                internal_authenticator,
                auth_disabled,
                labels,
            } = tokio::time::timeout(
                BOUND,
                build_oop_serve_options(
                    &cfg,
                    "limits-gear",
                    INSTANCE,
                    Some("1.2.3".to_owned()),
                    Duration::from_secs(5),
                    Arc::clone(&directory),
                ),
            )
            .await
            .expect("options deadline")
            .expect("options build");
            assert_eq!(
                (
                    (
                        gear_name,
                        instance_id,
                        version,
                        advertise_uri,
                        listen_addr,
                        probe_bind_addr
                    ),
                    (
                        drain_timeout,
                        heartbeat_interval,
                        healthcheck_timeout,
                        request_limits
                    ),
                    (
                        auth_disabled,
                        bearer_authenticator.is_some(),
                        internal_authenticator.is_some(),
                        labels
                    ),
                ),
                (
                    (
                        "limits-gear".to_owned(),
                        INSTANCE.to_string(),
                        Some("1.2.3".to_owned()),
                        "http://127.0.0.1:8000".to_owned(),
                        "127.0.0.1:8080".parse().expect("listen address"),
                        Some("127.0.0.1:8081".parse().expect("probe address"))
                    ),
                    (
                        Duration::from_secs(17),
                        Duration::from_secs(5),
                        Duration::from_millis(777),
                        expected_limits
                    ),
                    (false, false, false, std::collections::BTreeMap::new()),
                )
            );
            assert!(Arc::ptr_eq(&actual_directory, &directory));
        }
    }
}
