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
// advertise_uri fail-fast (`cpt-cf-adr-instance-addressable-discovery` §5)
// =============================================================================

mod advertise_uri_reachability {
    use super::*;

    #[test]
    fn bind_only_addresses_are_unreachable() {
        for uri in [
            "http://127.0.0.1:8080",
            "http://localhost:8080",
            "http://0.0.0.0:8080",
            "http://[::1]:8080",
            "http://[::]:8080",
            "https://LOCALHOST:8443",
        ] {
            assert!(
                !advertise_uri_is_reachable(uri),
                "{uri} should be treated as bind-only / unreachable"
            );
        }
    }

    #[test]
    fn routable_addresses_are_reachable() {
        for uri in [
            // k8s pod FQDN / Service DNS
            "http://ingest-0.ingest.default.svc:8080",
            "http://billing.default.svc.cluster.local:8080",
            // a concrete non-loopback IP
            "http://10.1.2.3:8080",
            "http://[2001:db8::1]:8080",
            // UDS is inherently instance-addressable (single-node Profile 2)
            "unix:///run/toolkit/ingest.sock",
        ] {
            assert!(
                advertise_uri_is_reachable(uri),
                "{uri} should be treated as reachable"
            );
        }
    }

    async fn serve_options_with(
        advertise_uri: Option<&str>,
        require_reachable: bool,
    ) -> Result<OopServeOptions> {
        let cfg = crate::bootstrap::config::OopHttpConfig {
            listen_addr: "0.0.0.0:8080".to_owned(),
            probe_bind_addr: None,
            drain_timeout_secs: 30,
            healthcheck_timeout_ms: 500,
            advertise_uri: advertise_uri.map(ToOwned::to_owned),
            require_reachable_advertise_uri: require_reachable,
            labels: std::collections::BTreeMap::new(),
            internal_auth: None,
        };
        let dir: Arc<dyn DirectoryClient> = Arc::new(crate::directory::LocalDirectoryClient::new(
            Arc::new(crate::runtime::GearManager::new()),
        ));
        build_oop_serve_options(
            &cfg,
            "svc",
            Uuid::new_v4(),
            None,
            Duration::from_secs(5),
            dir,
        )
        .await
    }

    #[tokio::test]
    async fn fails_fast_on_loopback_default_when_required() {
        // advertise_uri unset -> defaults to loopback (0.0.0.0 rewritten to
        // 127.0.0.1); with the flag set this MUST refuse to start.
        let err = serve_options_with(None, true).await.unwrap_err();
        assert!(
            err.to_string().contains("advertise_uri"),
            "expected a fail-fast advertise_uri error, got: {err}"
        );
    }

    #[tokio::test]
    async fn starts_with_reachable_uri_when_required() {
        let opts = serve_options_with(Some("http://svc.default.svc:8080"), true)
            .await
            .expect("reachable advertise_uri should start");
        assert_eq!(opts.advertise_uri, "http://svc.default.svc:8080");
    }

    #[tokio::test]
    async fn loopback_is_allowed_when_not_required() {
        // Default (flag off) preserves Profile 1 / local-dev behaviour.
        let opts = serve_options_with(None, false)
            .await
            .expect("loopback default is fine when not required");
        assert!(opts.advertise_uri.contains("127.0.0.1"));
    }
}

// =============================================================================
// role-qualified-name closed-set guard (`cpt-cf-adr-instance-addressable-discovery` §1)
// =============================================================================

mod role_name_guard {
    use super::*;

    #[test]
    fn none_is_a_single_role_gear_and_always_passes() {
        assert!(validate_role_name("anything", None).is_ok());
    }

    #[test]
    fn a_member_of_the_declared_set_passes() {
        let roles = [
            "event-broker".to_owned(),
            "event-broker-ingest".to_owned(),
            "event-broker-delivery".to_owned(),
        ];
        assert!(validate_role_name("event-broker-ingest", Some(&roles)).is_ok());
        assert!(validate_role_name("event-broker", Some(&roles)).is_ok());
    }

    #[test]
    fn a_name_outside_the_declared_set_is_rejected() {
        let roles = ["event-broker".to_owned(), "event-broker-ingest".to_owned()];
        // A mis-set boot mode producing an undeclared name must fail fast.
        let err = validate_role_name("event-broker-typo", Some(&roles)).unwrap_err();
        assert!(
            err.to_string()
                .contains("not in the declared role-name set")
        );
    }

    #[test]
    fn effective_role_names_prefers_explicit_override() {
        // An explicit `OopRunOptions.role_names` wins over discovery.
        let explicit = vec!["a".to_owned(), "b".to_owned()];
        let discovered = vec!["x".to_owned()];
        assert_eq!(
            effective_role_names(Some(explicit.clone()), discovered),
            Some(explicit)
        );
    }

    #[test]
    fn effective_role_names_falls_back_to_discovered() {
        // No explicit set -> use the inventory-discovered role set.
        let discovered = vec!["event-broker".to_owned(), "event-broker-ingest".to_owned()];
        assert_eq!(
            effective_role_names(None, discovered.clone()),
            Some(discovered)
        );
    }

    #[test]
    fn effective_role_names_none_when_nothing_declared() {
        // No explicit set and no gear declared roles -> single-role, no constraint.
        assert_eq!(effective_role_names(None, Vec::new()), None);
    }

    #[test]
    fn two_linked_gears_do_not_cross_validate() {
        // Two role-split gears linked into one binary. Each declares its own
        // closed set; selection must key on the declaring identity so a name is
        // validated against *its own* gear's roles, never the cross-gear union
        // (`cpt-cf-adr-instance-addressable-discovery` §1).
        let broker: (&str, &[&str]) = ("broker", &["broker", "broker-ingest"]);
        let cluster: (&str, &[&str]) = ("cluster", &["cluster", "cluster-follower"]);
        let decls = [broker, cluster];

        // A booted name selects ONLY its own gear's declared set.
        let broker_roles =
            crate::registry::select_roles_for(decls.iter().copied(), "broker-ingest").unwrap();
        assert_eq!(
            broker_roles,
            vec!["broker".to_owned(), "broker-ingest".to_owned()]
        );
        assert!(!broker_roles.contains(&"cluster".to_owned()));

        let cluster_roles =
            crate::registry::select_roles_for(decls.iter().copied(), "cluster-follower").unwrap();
        assert_eq!(
            cluster_roles,
            vec!["cluster".to_owned(), "cluster-follower".to_owned()]
        );

        // The booted name passes against its own gear's roles ...
        assert!(validate_role_name("broker-ingest", Some(&broker_roles)).is_ok());
        // ... but must NOT validate against the *other* gear's roles: a union
        // would have accepted it, per-gear selection rejects the cross-check.
        assert!(validate_role_name("broker-ingest", Some(&cluster_roles)).is_err());

        // A name declared by neither gear belongs to no role-split gear -> no
        // constraint (single-role / non-role gear), rather than being wrongly
        // constrained by another gear's roles.
        assert_eq!(
            crate::registry::select_roles_for(decls.iter().copied(), "unrelated"),
            None
        );
    }
}

// =============================================================================
// instance labels: config -> OopServeOptions (`cpt-cf-adr-instance-addressable-discovery` §2)
// =============================================================================

mod instance_labels {
    use super::*;

    fn cfg_with(
        labels: std::collections::BTreeMap<String, String>,
    ) -> crate::bootstrap::config::OopHttpConfig {
        crate::bootstrap::config::OopHttpConfig {
            listen_addr: "0.0.0.0:8080".to_owned(),
            probe_bind_addr: None,
            drain_timeout_secs: 30,
            healthcheck_timeout_ms: 500,
            advertise_uri: None,
            require_reachable_advertise_uri: false,
            labels,
            internal_auth: None,
        }
    }

    async fn serve_options_for(cfg: &crate::bootstrap::config::OopHttpConfig) -> OopServeOptions {
        let dir: Arc<dyn DirectoryClient> = Arc::new(crate::directory::LocalDirectoryClient::new(
            Arc::new(crate::runtime::GearManager::new()),
        ));
        build_oop_serve_options(
            cfg,
            "event-broker-ingest",
            Uuid::new_v4(),
            None,
            Duration::from_secs(5),
            dir,
        )
        .await
        .expect("build should succeed")
    }

    #[tokio::test]
    async fn config_labels_thread_through_to_serve_options() {
        // Config labels surface verbatim on OopServeOptions.labels (which the
        // presence loop registers with the directory).
        let cfg = cfg_with(std::collections::BTreeMap::from([
            ("role".to_owned(), "ingest".to_owned()),
            ("shard".to_owned(), "3".to_owned()),
        ]));
        let opts = serve_options_for(&cfg).await;
        assert_eq!(opts.labels.get("role").map(String::as_str), Some("ingest"));
        assert_eq!(opts.labels.get("shard").map(String::as_str), Some("3"));
    }

    #[tokio::test]
    async fn no_labels_is_empty() {
        let opts = serve_options_for(&cfg_with(std::collections::BTreeMap::new())).await;
        assert!(opts.labels.is_empty());
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
}
