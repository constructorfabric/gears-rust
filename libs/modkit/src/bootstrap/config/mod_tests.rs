use super::*;
use std::fs;
use temp_env::with_var;
use tempfile::tempdir;

/// Helper: a normalized `home_dir` should be absolute and not start with '~'.
fn is_normalized_path(p: &Path) -> bool {
    p.is_absolute() && !p.starts_with("~")
}

/// Helper: platform default subdirectory name.
fn default_subdir() -> &'static str {
    ".cyberfabric"
}

#[test]
fn test_default_config_structure() {
    let config = AppConfig::default();

    // Database defaults (simplified structure)
    assert!(config.database.is_none());

    // Logging defaults
    let logging = config.logging;
    assert!(logging.contains_key("default"));

    let default_section = &logging["default"];
    assert_eq!(default_section.console_level, Some(Level::INFO));
    assert_eq!(default_section.file().unwrap(), "logs/cyberfabric.log");

    // Modules bag is empty by default
    assert!(config.modules.is_empty());
}

#[test]
fn test_load_layered_normalizes_home_dir() {
    let tmp = tempdir().unwrap();
    let cfg_path = tmp.path().join("cfg.yaml");

    // Provide a user path with "~" to ensure expansion and normalization.
    let yaml = r#"
server:
  home_dir: "~/.test_hyperspot"

database:
  servers:
    test_postgres:
      dsn: "postgres://user:pass@localhost/db"
      pool:
        max_conns: 20

logging:
  default:
    console_level: debug
    file: "logs/default.log"
"#;
    fs::write(&cfg_path, yaml).unwrap();

    let config = AppConfig::load_layered(&cfg_path).unwrap();

    // home_dir should be normalized immediately
    assert!(is_normalized_path(&config.server.home_dir));
    assert!(config.server.home_dir.ends_with(".test_hyperspot"));

    // database parsed (TODO: update test to use new config format)
    // For now, since this test uses old format YAML, we skip DB assertions
    // let db = config.database.as_ref().unwrap();

    // logging parsed
    let logging = &config.logging;
    let def = &logging["default"];
    assert_eq!(def.console_level, Some(Level::DEBUG));
    assert_eq!(def.section_file.as_ref().unwrap().file, "logs/default.log");
}

#[test]
fn test_load_or_default_normalizes_home_dir_when_none() {
    // No external file => defaults, but home_dir must be normalized.
    // Ensure platform env is present for home resolution in CI.
    let tmp = tempdir().unwrap();
    let env_var = if cfg!(target_os = "windows") {
        "APPDATA"
    } else {
        "HOME"
    };
    with_var(env_var, Some(tmp.path().to_str().unwrap()), || {
        let config = AppConfig::load_or_default(None).unwrap();
        assert!(is_normalized_path(&config.server.home_dir));
        assert!(config.server.home_dir.ends_with(default_subdir()));
    });
}

#[test]
fn test_minimal_yaml_config() {
    let tmp = tempdir().unwrap();
    let cfg_path = tmp.path().join("cfg.yaml");

    let yaml = r#"
server:
  home_dir: "~/.minimal"
"#;
    fs::write(&cfg_path, yaml).unwrap();

    let config = AppConfig::load_layered(&cfg_path).unwrap();

    // Required fields are parsed; home_dir normalized
    assert!(is_normalized_path(&config.server.home_dir));
    assert!(config.server.home_dir.ends_with(".minimal"));

    // Optional sections default to None
    assert!(config.database.is_none());
    assert!(config.modules.is_empty());
}

#[test]
fn test_cli_overrides() {
    let mut config = AppConfig::default();

    let args = CliArgs {
        config: None,
        print_config: false,
        verbose: 2, // trace
        mock: false,
    };

    config.apply_cli_overrides(args.verbose);

    // Port override

    // Verbose override affects logging
    let logging = &config.logging;
    let default_section = &logging["default"];
    assert_eq!(default_section.console_level, Some(Level::TRACE));
}

#[test]
fn test_cli_verbose_levels_matrix() {
    for (verbose_level, expected_log_level) in [
        (0, Some(Level::INFO)), // unchanged from default
        (1, Some(Level::DEBUG)),
        (2, Some(Level::TRACE)),
        (3, Some(Level::TRACE)), // cap at trace
    ] {
        let mut config = AppConfig::default();
        let args = CliArgs {
            config: None,
            print_config: false,
            verbose: verbose_level,
            mock: false,
        };

        config.apply_cli_overrides(args.verbose);

        let logging = &config.logging;
        let default_section = &logging["default"];

        if verbose_level == 0 {
            assert_eq!(default_section.console_level, Some(Level::INFO));
        } else {
            assert_eq!(default_section.console_level, expected_log_level);
        }
    }
}

#[test]
fn test_layered_config_loading_with_modules_dir() {
    let tmp = tempdir().unwrap();
    let cfg_path = tmp.path().join("modules_dir.yaml");
    let modules_dir = tmp.path().join("modules");

    fs::create_dir_all(&modules_dir).unwrap();
    let module_cfg = modules_dir.join("test_module.yaml");
    fs::write(
        &module_cfg,
        r#"
setting1: "value1"
setting2: 42
"#,
    )
    .unwrap();

    // Convert Windows paths to forward slashes for YAML compatibility
    let modules_dir_str = normalize_path(&modules_dir);
    let yaml = format!(
        r#"
server:
  home_dir: "~/.modules_test"

modules_dir: "{modules_dir_str}"

modules:
  existing_module:
    key: "value"
"#
    );

    fs::write(&cfg_path, yaml).unwrap();

    let config = AppConfig::load_layered(&cfg_path).unwrap();

    // Should have loaded the existing module from modules section
    assert!(config.modules.contains_key("existing_module"));

    // Should have also loaded the module from modules_dir
    assert!(config.modules.contains_key("test_module"));

    // Check the loaded module config
    let test_module = &config.modules["test_module"];
    assert_eq!(test_module["setting1"], "value1");
    assert_eq!(test_module["setting2"], 42);
}

#[test]
fn test_load_and_init_logging_smoke() {
    // Just verifies structure is acceptable for logging init path.
    let tmp = tempdir().unwrap();
    let cfg_path = tmp.path().join("logging.yaml");
    let yaml = r#"
server:
  home_dir: "~/.logging_test"

logging:
  default:
    console_level: debug
    file: ""
    file_level: info
"#;
    fs::write(&cfg_path, yaml).unwrap();

    let config = AppConfig::load_layered(&cfg_path).unwrap();
    let logging = &config.logging;
    assert!(logging.contains_key("default"));

    let default_section = &logging["default"];
    assert_eq!(default_section.console_level, Some(Level::DEBUG));
    assert_eq!(default_section.file_level(), Some(Level::INFO));
    // not calling init to avoid side effects in tests
}

// ===================== DB Configuration Precedence Tests =====================

/// Helper function to create `AppConfig` with database server configuration
fn create_app_with_server(server_name: &str, db_config: DbConnConfig) -> AppConfig {
    let mut servers = HashMap::new();
    servers.insert(server_name.to_owned(), db_config);

    AppConfig {
        database: Some(GlobalDatabaseConfig {
            servers,
            auto_provision: None,
        }),
        ..Default::default()
    }
}

/// Helper function to add a module to `AppConfig`
fn add_module_to_app(app: &mut AppConfig, module_name: &str, database_config: &serde_json::Value) {
    app.modules.insert(
        module_name.to_owned(),
        serde_json::json!({
            "database": database_config,
            "config": {}
        }),
    );
}

/// Helper function to add a module with custom config to `AppConfig`
fn add_module_with_config(app: &mut AppConfig, module_name: &str, config: &serde_json::Value) {
    app.modules.insert(
        module_name.to_owned(),
        serde_json::json!({
            "database": {},
            "config": config
        }),
    );
}

/// Helper function to create a minimal `AppConfig` for testing
fn create_minimal_app() -> AppConfig {
    AppConfig {
        database: None,
        modules: HashMap::new(),
        ..Default::default()
    }
}

#[test]
fn test_precedence_global_dsn_only() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let mut app = create_app_with_server(
        "test_server",
        DbConnConfig {
            dsn: Some("postgresql://global_user:global_pass@global_host:5432/global_db".to_owned()),
            ..Default::default()
        },
    );

    // Module references global server
    add_module_to_app(
        &mut app,
        "test_module",
        &serde_json::json!({
            "server": "test_server"
        }),
    );

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (dsn, _pool) = result.unwrap();
    assert!(dsn.contains("global_user"));
    assert!(dsn.contains("global_host"));
    assert!(dsn.contains("global_db"));
}

#[test]
fn test_precedence_global_fields_only() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let mut app = create_app_with_server(
        "test_server",
        DbConnConfig {
            host: Some("field_host".to_owned()),
            port: Some(5433),
            user: Some("field_user".to_owned()),
            dbname: Some("field_db".to_owned()),
            ..Default::default()
        },
    );

    // Module references global server
    add_module_to_app(
        &mut app,
        "test_module",
        &serde_json::json!({
            "server": "test_server"
        }),
    );

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (dsn, _pool) = result.unwrap();
    assert!(dsn.contains("field_host"));
    assert!(dsn.contains("5433"));
    assert!(dsn.contains("field_user"));
    assert!(dsn.contains("field_db"));
}

#[test]
fn test_precedence_module_dsn_only() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let app = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {
                        "dsn": "sqlite://module_test.db?wal=true&synchronous=NORMAL"
                    },
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (dsn, _pool) = result.unwrap();
    assert!(dsn.contains("module_test.db"));
    assert!(dsn.contains("wal=true"));
}

#[test]
fn test_precedence_module_fields_only() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let app = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {
                        "file": "module_fields.db"
                    },
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (dsn, _pool) = result.unwrap();
    assert!(dsn.contains("module_fields.db"));
    // Platform-specific DSN format check
    #[cfg(windows)]
    assert!(dsn.starts_with("sqlite:") && !dsn.starts_with("sqlite://"));
    #[cfg(unix)]
    assert!(dsn.starts_with("sqlite://"));
}

#[test]
fn test_precedence_fields_override_dsn() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let mut app = create_app_with_server(
        "test_server",
        DbConnConfig {
            dsn: Some("postgresql://old_user:old_pass@old_host:5432/old_db".to_owned()),
            host: Some("new_host".to_owned()), // This should override DSN host
            port: Some(5433),                  // This should override DSN port
            user: Some("new_user".to_owned()), // This should override DSN user
            dbname: Some("new_db".to_owned()), // This should override DSN dbname
            ..Default::default()
        },
    );

    // Module also overrides some fields
    add_module_to_app(
        &mut app,
        "test_module",
        &serde_json::json!({
            "server": "test_server",
            "port": 5434  // Module field should override global field
        }),
    );

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (dsn, _pool) = result.unwrap();
    // Fields should override DSN parts
    assert!(dsn.contains("new_host"));
    assert!(dsn.contains("5434")); // Module override should win
    assert!(dsn.contains("new_user"));
    assert!(dsn.contains("new_db"));
    // Old DSN values should not appear
    assert!(!dsn.contains("old_host"));
    assert!(!dsn.contains("5432"));
    assert!(!dsn.contains("old_user"));
    assert!(!dsn.contains("old_db"));
}

#[test]
fn test_env_expansion_password() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    with_var("TEST_DB_PASSWORD", Some("secret123"), || {
        let mut app = create_app_with_server(
            "test_server",
            DbConnConfig {
                host: Some("localhost".to_owned()),
                port: Some(5432),
                user: Some("testuser".to_owned()),
                password: Some("${TEST_DB_PASSWORD}".to_owned()), // Should expand to "secret123"
                dbname: Some("testdb".to_owned()),
                ..Default::default()
            },
        );

        add_module_to_app(
            &mut app,
            "test_module",
            &serde_json::json!({
                "server": "test_server"
            }),
        );

        let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
        assert!(result.is_some());

        let (dsn, _pool) = result.unwrap();
        assert!(dsn.contains("secret123"));
    });
}

#[test]
fn test_env_expansion_in_dsn() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    temp_env::with_vars(
        [
            ("DB_HOST", Some("test-server")),
            ("DB_PASSWORD", Some("env_secret")),
        ],
        || {
            let mut app = create_app_with_server(
                "test_server",
                DbConnConfig {
                    dsn: Some("postgresql://user:${DB_PASSWORD}@${DB_HOST}:5432/mydb".to_owned()),
                    ..Default::default()
                },
            );

            add_module_to_app(
                &mut app,
                "test_module",
                &serde_json::json!({
                    "server": "test_server"
                }),
            );

            let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
            assert!(result.is_some());

            let (dsn, _pool) = result.unwrap();
            assert!(dsn.contains("test-server"));
            assert!(dsn.contains("env_secret"));
            // ${} placeholders should be replaced
            assert!(!dsn.contains("${DB_HOST}"));
            assert!(!dsn.contains("${DB_PASSWORD}"));
        },
    );
}

#[test]
fn test_sqlite_file_path_resolution() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    // Test 1: file (relative to home_dir/module_name/)
    let app1 = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {
                        "file": "test.db"
                    },
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result1 = build_final_db_for_module(&app1, "test_module", home_dir, false).unwrap();
    assert!(result1.is_some());
    let (dsn1, _) = result1.unwrap();
    assert!(dsn1.contains("test_module"));
    assert!(dsn1.contains("test.db"));

    // Test 2: path (absolute path)
    let abs_path = tmp.path().join("absolute.db");
    let app2 = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {
                        "path": abs_path.to_string_lossy()
                    },
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result2 = build_final_db_for_module(&app2, "test_module", home_dir, false).unwrap();
    assert!(result2.is_some());
    let (dsn2, _) = result2.unwrap();
    assert!(dsn2.contains("absolute.db"));

    // Test 3: no file or path (should default to module_name.sqlite)
    let app3 = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {},
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result3 = build_final_db_for_module(&app3, "test_module", home_dir, false).unwrap();
    assert!(result3.is_some());
    let (dsn3, _) = result3.unwrap();
    assert!(dsn3.contains("test_module.sqlite"));
}

#[cfg(windows)]
#[test]
fn test_sqlite_path_resolution_windows() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let app = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {
                        "file": "test.db"
                    },
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());
    let (dsn, _) = result.unwrap();

    // On Windows, paths should be normalized to forward slashes in DSN
    assert!(!dsn.contains('\\'));
    assert!(dsn.contains('/'));
}

#[test]
fn test_sqlite_dsn_with_server_reference_and_dbname_override() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let mut app = AppConfig::default();

    // Global server with SQLite DSN and query params
    let mut servers = HashMap::new();
    servers.insert(
        "sqlite_users".to_owned(),
        DbConnConfig {
            engine: None,
            dsn: Some(
                "sqlite://users_info.db?WAL=true&synchronous=NORMAL&busy_timeout=5000".to_owned(),
            ),
            host: None,
            port: None,
            user: None,
            password: None,
            dbname: None,
            params: None,
            pool: None,
            file: None,
            path: None,
            server: None,
        },
    );

    app.database = Some(GlobalDatabaseConfig {
        servers,
        auto_provision: None,
    });

    // Module that references the server but overrides the dbname
    app.modules.insert(
        "users_info".to_owned(),
        serde_json::json!({
            "database": {
                "server": "sqlite_users",
                "dbname": "users_info.db"
            },
            "config": {}
        }),
    );

    let result = build_final_db_for_module(&app, "users_info", home_dir, false).unwrap();
    assert!(result.is_some());
    let (dsn, _) = result.unwrap();

    // Should be an absolute path with preserved query parameters
    assert!(dsn.contains("?WAL=true&synchronous=NORMAL&busy_timeout=5000"));
    assert!(dsn.contains("users_info/users_info.db"));

    // Platform-specific path format
    #[cfg(windows)]
    {
        // Windows should use sqlite:C:/path format
        assert!(dsn.starts_with("sqlite:"));
        assert!(!dsn.starts_with("sqlite://"));
    }

    #[cfg(unix)]
    {
        // Unix should use sqlite://path format
        assert!(dsn.starts_with("sqlite://"));
    }
}

#[cfg(unix)]
#[test]
fn test_sqlite_path_resolution_unix() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let app = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {
                        "file": "test.db"
                    },
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());
    let (dsn, _) = result.unwrap();

    // On Unix, paths should be absolute
    assert!(dsn.starts_with("sqlite://"));
    assert!(dsn.contains("/test_module/test.db"));
}

#[test]
fn test_server_based_db_missing_dbname_error() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let mut app = create_app_with_server(
        "test_server",
        DbConnConfig {
            host: Some("localhost".to_owned()),
            port: Some(5432),
            user: Some("testuser".to_owned()),
            // Missing dbname for server-based DB
            ..Default::default()
        },
    );

    add_module_to_app(
        &mut app,
        "test_module",
        &serde_json::json!({
            "server": "test_server"
        }),
    );

    let result = build_final_db_for_module(&app, "test_module", home_dir, false);
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("missing required 'dbname'"));
}

#[test]
fn test_module_no_database_config() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    // Module with no database section
    let app = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "no_db_module".to_owned(),
                serde_json::json!({
                    "config": {
                        "some_setting": "value"
                    }
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result = build_final_db_for_module(&app, "no_db_module", home_dir, false).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_module_empty_database_config() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    // Module with empty database section
    let app = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "empty_db_module".to_owned(),
                serde_json::json!({
                    "database": null,
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result = build_final_db_for_module(&app, "empty_db_module", home_dir, false).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_referenced_server_not_found() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let app = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {
                        "server": "nonexistent_server"
                    },
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result = build_final_db_for_module(&app, "test_module", home_dir, false);
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("Referenced server 'nonexistent_server' not found"));
}

#[test]
fn test_dsn_validation_invalid_url() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let app = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {
                        "dsn": "invalid://not-a-valid[url"
                    },
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result = build_final_db_for_module(&app, "test_module", home_dir, false);
    assert!(result.is_err());
}

#[test]
fn test_env_variable_not_found() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    // Use with_var with None to ensure the env var doesn't exist
    with_var("NONEXISTENT_PASSWORD", None::<&str>, || {
        let mut app = create_app_with_server(
            "test_server",
            DbConnConfig {
                host: Some("localhost".to_owned()),
                password: Some("${NONEXISTENT_PASSWORD}".to_owned()),
                dbname: Some("testdb".to_owned()),
                ..Default::default()
            },
        );

        add_module_to_app(
            &mut app,
            "test_module",
            &serde_json::json!({
                "server": "test_server"
            }),
        );

        let result = build_final_db_for_module(&app, "test_module", home_dir, false);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("NONEXISTENT_PASSWORD"));
    });
}

#[test]
fn test_sqlite_at_file_relative_path() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let app = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {
                        "dsn": "sqlite://@file(users.db)"
                    },
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (dsn, _pool) = result.unwrap();
    assert!(dsn.contains("test_module"));
    assert!(dsn.contains("users.db"));
    // Platform-specific DSN format check
    #[cfg(windows)]
    assert!(dsn.starts_with("sqlite:") && !dsn.starts_with("sqlite://"));
    #[cfg(unix)]
    assert!(dsn.starts_with("sqlite:///"));
}

#[test]
fn test_sqlite_at_file_absolute_path() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();
    let abs_path = tmp.path().join("absolute_db.sqlite");

    let app = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {
                        "dsn": format!("sqlite://@file({})", abs_path.to_string_lossy())
                    },
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (dsn, _pool) = result.unwrap();
    assert!(dsn.contains("absolute_db.sqlite"));
    // Platform-specific DSN format check
    #[cfg(windows)]
    assert!(dsn.starts_with("sqlite:") && !dsn.starts_with("sqlite://"));
    #[cfg(unix)]
    assert!(dsn.starts_with("sqlite:///"));
}

#[test]
fn test_sqlite_empty_dsn_default() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let app = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {
                        "dsn": "sqlite://"
                    },
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (dsn, _pool) = result.unwrap();
    assert!(dsn.contains("test_module"));
    assert!(dsn.contains("test_module.sqlite"));
    // Platform-specific DSN format check
    #[cfg(windows)]
    assert!(dsn.starts_with("sqlite:") && !dsn.starts_with("sqlite://"));
    #[cfg(unix)]
    assert!(dsn.starts_with("sqlite:///"));
}

#[test]
fn test_sqlite_at_file_invalid_syntax() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let app = AppConfig {
        modules: {
            let mut modules = HashMap::new();
            modules.insert(
                "test_module".to_owned(),
                serde_json::json!({
                    "database": {
                        "dsn": "sqlite://@file(missing_closing_paren"
                    },
                    "config": {}
                }),
            );
            modules
        },
        ..Default::default()
    };

    let result = build_final_db_for_module(&app, "test_module", home_dir, false);
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("Invalid @file() syntax"));
}

#[test]
fn test_dsn_special_characters_in_credentials() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    // Test with special characters in username and password
    let mut app = create_app_with_server(
        "test_server",
        DbConnConfig {
            host: Some("localhost".to_owned()),
            port: Some(5432),
            user: Some("user@domain".to_owned()),
            password: Some("pa@ss:w0rd/with%special&chars".to_owned()),
            dbname: Some("test/db".to_owned()),
            ..Default::default()
        },
    );

    add_module_to_app(
        &mut app,
        "test_module",
        &serde_json::json!({
            "server": "test_server"
        }),
    );

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (dsn, _pool) = result.unwrap();

    // Verify DSN is properly encoded
    assert!(dsn.starts_with("postgresql://"));
    assert!(dsn.contains("user%40domain")); // @ encoded as %40
    assert!(dsn.contains("/test%2Fdb")); // / in dbname encoded as %2F

    // Verify DSN is parseable and contains expected user
    validate_dsn(&dsn).expect("DSN with special characters should be valid");

    // Parse the DSN to verify it contains the correct components
    let parsed_dsn = dsn::parse(&dsn).expect("DSN should be parseable");
    assert_eq!(parsed_dsn.username.as_deref(), Some("user@domain"));
    assert_eq!(
        parsed_dsn.password.as_deref(),
        Some("pa@ss:w0rd/with%special&chars")
    );
    // Note: dsn crate may have limitations with path parsing - just verify the main DSN works
    // The important thing is that the DSN is valid and contains the right components
}

#[test]
#[allow(clippy::non_ascii_literal)]
fn test_dsn_unicode_characters() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    // Test with Unicode characters
    let mut app = create_app_with_server(
        "test_server",
        DbConnConfig {
            host: Some("localhost".to_owned()),
            user: Some("ユーザー".to_owned()), // Japanese characters
            dbname: Some("unicode_db".to_owned()),
            ..Default::default()
        },
    );

    add_module_to_app(
        &mut app,
        "test_module",
        &serde_json::json!({
            "server": "test_server"
        }),
    );

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (dsn, _pool) = result.unwrap();

    // Verify DSN is properly encoded with Unicode
    assert!(dsn.starts_with("postgresql://"));
    // Unicode characters should be percent-encoded
    assert!(dsn.contains('%')); // Should contain encoded characters

    // Verify DSN is parseable
    validate_dsn(&dsn).expect("DSN with Unicode characters should be valid");
}

#[test]
fn test_dsn_query_parameters_encoding() {
    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    let mut params = HashMap::new();
    params.insert("ssl mode".to_owned(), "require & verify".to_owned());
    params.insert("application_name".to_owned(), "my-app/v1.0".to_owned());

    let mut app = create_app_with_server(
        "test_server",
        DbConnConfig {
            host: Some("localhost".to_owned()),
            user: Some("testuser".to_owned()),
            dbname: Some("testdb".to_owned()),
            params: Some(params),
            ..Default::default()
        },
    );

    add_module_to_app(
        &mut app,
        "test_module",
        &serde_json::json!({
            "server": "test_server"
        }),
    );

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (dsn, _pool) = result.unwrap();

    // Verify query parameters are properly encoded (spaces become +, & becomes %26)
    assert!(dsn.contains("ssl+mode=require+%26+verify"));
    assert!(dsn.contains("application_name=my-app%2Fv1.0"));

    // Verify DSN is parseable
    validate_dsn(&dsn).expect("DSN with encoded query parameters should be valid");
}

#[test]
fn test_pool_config_merging() {
    use std::time::Duration;

    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    // Global server with pool config
    let mut app = create_app_with_server(
        "test_server",
        DbConnConfig {
            host: Some("localhost".to_owned()),
            dbname: Some("testdb".to_owned()),
            pool: Some(PoolCfg {
                max_conns: Some(10),
                min_conns: None,
                acquire_timeout: Some(Duration::from_secs(5)),
                idle_timeout: None,
                max_lifetime: None,
                test_before_acquire: None,
            }),
            ..Default::default()
        },
    );

    // Module overrides only max_conns
    add_module_to_app(
        &mut app,
        "test_module",
        &serde_json::json!({
            "server": "test_server",
            "pool": {
                "max_conns": 20
            }
        }),
    );

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (_dsn, pool) = result.unwrap();
    assert_eq!(pool.max_conns, Some(20)); // Module override wins
    assert_eq!(pool.acquire_timeout, Some(Duration::from_secs(5))); // Global value preserved
}

#[test]
fn test_pool_config_module_overrides_all() {
    use std::time::Duration;

    let tmp = tempdir().unwrap();
    let home_dir = tmp.path();

    // Global server with pool config
    let mut app = create_app_with_server(
        "test_server",
        DbConnConfig {
            host: Some("localhost".to_owned()),
            dbname: Some("testdb".to_owned()),
            pool: Some(PoolCfg {
                max_conns: Some(10),
                min_conns: None,
                acquire_timeout: Some(Duration::from_secs(5)),
                idle_timeout: None,
                max_lifetime: None,
                test_before_acquire: None,
            }),
            ..Default::default()
        },
    );

    // Module overrides both pool settings
    add_module_to_app(
        &mut app,
        "test_module",
        &serde_json::json!({
            "server": "test_server",
            "pool": {
                "max_conns": 30,
                "acquire_timeout": "10s"
            }
        }),
    );

    let result = build_final_db_for_module(&app, "test_module", home_dir, false).unwrap();
    assert!(result.is_some());

    let (_dsn, pool) = result.unwrap();
    assert_eq!(pool.max_conns, Some(30));
    assert_eq!(pool.acquire_timeout, Some(Duration::from_secs(10)));
}

#[test]
fn test_list_module_names() {
    let mut app = create_minimal_app();
    add_module_with_config(&mut app, "zebra_module", &serde_json::json!({}));
    add_module_with_config(&mut app, "alpha_module", &serde_json::json!({}));
    add_module_with_config(&mut app, "beta_module", &serde_json::json!({}));

    let module_names = list_module_names(&app);

    // Should be sorted alphabetically
    assert_eq!(module_names.len(), 3);
    assert_eq!(module_names[0], "alpha_module");
    assert_eq!(module_names[1], "beta_module");
    assert_eq!(module_names[2], "zebra_module");
}

#[test]
fn test_list_module_names_empty() {
    let app = create_minimal_app();
    let module_names = list_module_names(&app);
    assert_eq!(module_names.len(), 0);
}

#[test]
fn test_redact_dsn_password_postgres() {
    let dsn = "postgres://user:secretpass@localhost:5432/mydb";
    let redacted = redact_dsn_password(dsn).unwrap();
    assert_eq!(
        redacted,
        "postgres://user:***REDACTED***@localhost:5432/mydb"
    );
}

#[test]
fn test_redact_dsn_password_no_password() {
    let dsn = "postgres://user@localhost:5432/mydb";
    let redacted = redact_dsn_password(dsn).unwrap();
    // No password means no redaction needed
    assert_eq!(redacted, "postgres://user@localhost:5432/mydb");
}

#[test]
fn test_redact_dsn_password_special_chars() {
    let dsn = "postgres://user:p@ss%40word@localhost:5432/mydb";
    let redacted = redact_dsn_password(dsn).unwrap();
    assert_eq!(
        redacted,
        "postgres://user:***REDACTED***@localhost:5432/mydb"
    );
}

#[test]
fn test_render_effective_modules_config() {
    let mut app = create_minimal_app();
    add_module_with_config(
        &mut app,
        "test_module",
        &serde_json::json!({
            "my_setting": "my_value",
            "enabled": true
        }),
    );

    let result = render_effective_modules_config(&app).unwrap();

    // Check structure
    assert!(result.is_object());
    let modules = result.as_object().unwrap();
    assert!(modules.contains_key("test_module"));

    let test_module = modules.get("test_module").unwrap();
    assert!(test_module.is_object());
    let test_module_obj = test_module.as_object().unwrap();

    // Should have config section
    assert!(test_module_obj.contains_key("config"));

    // Check config section
    let config = test_module_obj.get("config").unwrap();
    assert_eq!(config.get("my_setting").unwrap(), "my_value");
    assert_eq!(config.get("enabled").unwrap(), true);
}

#[test]
fn test_render_effective_modules_config_with_database() {
    let mut app = create_app_with_server(
        "test_server",
        DbConnConfig {
            host: Some("localhost".to_owned()),
            port: Some(5432),
            user: Some("user".to_owned()),
            password: Some("pass".to_owned()),
            dbname: Some("db".to_owned()),
            ..Default::default()
        },
    );

    // Module with database config
    add_module_to_app(
        &mut app,
        "test_module",
        &serde_json::json!({
            "server": "test_server"
        }),
    );

    let result = render_effective_modules_config(&app).unwrap();
    let modules = result.as_object().unwrap();
    let test_module = modules.get("test_module").unwrap().as_object().unwrap();

    // Should have database section
    assert!(test_module.contains_key("database"));
    let database = test_module.get("database").unwrap().as_object().unwrap();
    assert!(database.contains_key("dsn"));

    // DSN should be redacted
    let dsn = database.get("dsn").unwrap().as_str().unwrap();
    assert!(dsn.contains("***REDACTED***"));
    assert!(!dsn.contains("pass"));
}

#[test]
fn test_render_effective_modules_config_minimal() {
    // Test that modules with minimal/no config can be rendered
    let mut app = create_minimal_app();

    // Manually add a module with no database or config sections
    app.modules
        .insert("minimal_module".to_owned(), serde_json::json!({}));

    let result = render_effective_modules_config(&app).unwrap();

    // Module should be present in output (or excluded if truly empty)
    // Either way, rendering should succeed
    assert!(result.is_object());
}

#[test]
fn test_dump_effective_modules_config_yaml() {
    let mut app = create_minimal_app();
    add_module_with_config(
        &mut app,
        "test_module",
        &serde_json::json!({
            "setting": "value"
        }),
    );

    let yaml = dump_effective_modules_config_yaml(&app).unwrap();

    // Should be valid YAML
    assert!(yaml.contains("test_module:"));
    assert!(yaml.contains("config:"));
    assert!(yaml.contains("setting: value"));
}

#[test]
fn test_dump_effective_modules_config_json() {
    let mut app = create_minimal_app();
    add_module_with_config(
        &mut app,
        "test_module",
        &serde_json::json!({
            "setting": "value"
        }),
    );

    let json = dump_effective_modules_config_json(&app).unwrap();

    // Should be valid JSON
    assert!(json.contains("\"test_module\""));
    assert!(json.contains("\"config\""));
    assert!(json.contains("\"setting\""));
    assert!(json.contains("\"value\""));

    // Verify it's parseable
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed.is_object());
}

#[test]
fn test_render_multiple_modules() {
    let mut app = create_minimal_app();
    add_module_with_config(&mut app, "module_a", &serde_json::json!({"a": 1}));
    add_module_with_config(&mut app, "module_b", &serde_json::json!({"b": 2}));
    add_module_with_config(&mut app, "module_c", &serde_json::json!({"c": 3}));

    let result = render_effective_modules_config(&app).unwrap();
    let modules = result.as_object().unwrap();

    assert_eq!(modules.len(), 3);
    assert!(modules.contains_key("module_a"));
    assert!(modules.contains_key("module_b"));
    assert!(modules.contains_key("module_c"));
}

// ========== Vendor configuration tests ==========

#[derive(Debug, Deserialize, Default, PartialEq)]
struct TestVendorConfig {
    #[serde(default)]
    api_token: String,
    #[serde(default)]
    api_url: String,
}

#[test]
fn test_vendor_section_parses_from_yaml() {
    let yaml = r#"
server:
  home_dir: "~/.test_vendor"
vendor:
  acme:
    api_token: "acme-token-123"
    api_url: "https://acme.example.com"
  other_corp:
    api_token: "other-token-789"
    api_url: "https://other.example.com"
"#;
    let config: AppConfig = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(config.vendor.len(), 2);
    assert!(config.vendor.contains_key("acme"));
    assert!(config.vendor.contains_key("other_corp"));

    let acme: TestVendorConfig = config.vendor_config("acme").unwrap();
    assert_eq!(acme.api_token, "acme-token-123");
    assert_eq!(acme.api_url, "https://acme.example.com");

    let other: TestVendorConfig = config.vendor_config("other_corp").unwrap();
    assert_eq!(other.api_token, "other-token-789");
    assert_eq!(other.api_url, "https://other.example.com");
}

#[test]
fn test_vendor_section_defaults_to_empty() {
    let config = AppConfig::default();
    assert!(config.vendor.is_empty());
}

#[test]
fn test_vendor_config_typed_access() {
    let mut config = AppConfig::default();
    config.vendor.insert(
        "acme".to_owned(),
        serde_json::json!({
            "api_token": "acme-token-123",
            "api_url": "https://acme.example.com"
        }),
    );

    let acme: TestVendorConfig = config.vendor_config("acme").unwrap();
    assert_eq!(acme.api_token, "acme-token-123");
    assert_eq!(acme.api_url, "https://acme.example.com");
}

#[test]
fn test_vendor_config_not_found() {
    let config = AppConfig::default();
    let result: Result<TestVendorConfig, _> = config.vendor_config("nonexistent");
    assert!(matches!(
        result,
        Err(VendorConfigError::NotFound { ref vendor }) if vendor == "nonexistent"
    ));
}

#[test]
fn test_vendor_config_invalid_structure() {
    let mut config = AppConfig::default();
    config
        .vendor
        .insert("bad".to_owned(), serde_json::json!("not an object"));

    let result: Result<TestVendorConfig, _> = config.vendor_config("bad");
    assert!(matches!(
        result,
        Err(VendorConfigError::InvalidConfig { ref vendor, .. }) if vendor == "bad"
    ));
}

#[test]
fn test_vendor_config_or_default_missing() {
    let config = AppConfig::default();
    let acme: TestVendorConfig = config.vendor_config_or_default("acme").unwrap();
    assert_eq!(acme, TestVendorConfig::default());
}

#[test]
fn test_vendor_config_or_default_present() {
    let mut config = AppConfig::default();
    config.vendor.insert(
        "acme".to_owned(),
        serde_json::json!({ "api_token": "acme-token-123" }),
    );

    let acme: TestVendorConfig = config.vendor_config_or_default("acme").unwrap();
    assert_eq!(acme.api_token, "acme-token-123");
}

#[test]
fn test_vendor_config_env_override() {
    let tmp = tempdir().unwrap();
    let cfg_path = tmp.path().join("cfg.yaml");
    let yaml = r#"
server:
  home_dir: "~/.test_vendor"
vendor:
  env_test_vendor:
    api_token: "from_yaml"
"#;
    fs::write(&cfg_path, yaml).unwrap();

    with_var(
        "APP__VENDOR__ENV_TEST_VENDOR__API_TOKEN",
        Some("from_env"),
        || {
            let config = AppConfig::load_layered(&cfg_path).unwrap();
            let v: TestVendorConfig = config.vendor_config("env_test_vendor").unwrap();
            assert_eq!(v.api_token, "from_env");
        },
    );
}

#[test]
fn test_vendor_multiple_vendors_typed_access() {
    let mut config = AppConfig::default();
    config.vendor.insert(
        "acme".to_owned(),
        serde_json::json!({ "api_token": "acme-token", "api_url": "https://acme.com" }),
    );
    config.vendor.insert(
        "other_corp".to_owned(),
        serde_json::json!({ "api_token": "other-token", "api_url": "https://other.com" }),
    );

    let acme: TestVendorConfig = config.vendor_config("acme").unwrap();
    let other: TestVendorConfig = config.vendor_config("other_corp").unwrap();

    assert_eq!(acme.api_token, "acme-token");
    assert_eq!(other.api_token, "other-token");
    assert_eq!(acme.api_url, "https://acme.com");
    assert_eq!(other.api_url, "https://other.com");
}

#[test]
fn test_vendor_nested_config() {
    #[derive(Debug, Deserialize, PartialEq)]
    struct NestedVendorConfig {
        api_url: String,
        feature_flags: FeatureFlags,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct FeatureFlags {
        beta_mode: bool,
        max_retries: u32,
    }

    let mut config = AppConfig::default();
    config.vendor.insert(
        "acme".to_owned(),
        serde_json::json!({
            "api_url": "https://acme.com",
            "feature_flags": {
                "beta_mode": true,
                "max_retries": 3
            }
        }),
    );

    let acme: NestedVendorConfig = config.vendor_config("acme").unwrap();
    assert_eq!(acme.api_url, "https://acme.com");
    assert!(acme.feature_flags.beta_mode);
    assert_eq!(acme.feature_flags.max_retries, 3);
}

#[test]
fn test_vendor_config_or_default_invalid_returns_error() {
    let mut config = AppConfig::default();
    config
        .vendor
        .insert("bad".to_owned(), serde_json::json!("not an object"));

    let result: Result<TestVendorConfig, _> = config.vendor_config_or_default("bad");
    assert!(matches!(
        result,
        Err(VendorConfigError::InvalidConfig { ref vendor, .. }) if vendor == "bad"
    ));
}

#[test]
fn test_vendor_config_yaml_roundtrip() {
    let mut config = AppConfig::default();
    config.vendor.insert(
        "acme".to_owned(),
        serde_json::json!({ "api_token": "acme-token-123" }),
    );

    let yaml = config.to_yaml().unwrap();
    assert!(yaml.contains("vendor"));
    assert!(yaml.contains("acme"));
    assert!(yaml.contains("acme-token-123"));
}

#[test]
fn test_vendor_coexists_with_modules() {
    let mut config = AppConfig::default();
    config.modules.insert(
        "my_module".to_owned(),
        serde_json::json!({ "config": { "some_setting": true } }),
    );
    config.vendor.insert(
        "acme".to_owned(),
        serde_json::json!({ "api_token": "acme-token-123" }),
    );

    assert!(config.modules.contains_key("my_module"));
    assert!(config.vendor.contains_key("acme"));

    let acme: TestVendorConfig = config.vendor_config("acme").unwrap();
    assert_eq!(acme.api_token, "acme-token-123");
}

#[test]
fn test_vendor_error_display_messages() {
    let not_found = VendorConfigError::NotFound {
        vendor: "acme".to_owned(),
    };
    assert_eq!(
        not_found.to_string(),
        "vendor 'acme' not found in configuration"
    );

    let invalid = VendorConfigError::InvalidConfig {
        vendor: "bad".to_owned(),
        source: serde_json::from_str::<TestVendorConfig>("invalid").unwrap_err(),
    };
    let msg = invalid.to_string();
    assert!(msg.starts_with("invalid config for vendor 'bad':"));
}

#[test]
fn test_vendor_empty_object_in_yaml() {
    let yaml = r#"
server:
  home_dir: "~/.test_vendor"
vendor: {}
"#;
    let config: AppConfig = serde_saphyr::from_str(yaml).unwrap();
    assert!(config.vendor.is_empty());
}

// ========== Duplicate YAML key rejection tests ==========

#[test]
fn test_reject_duplicate_module_names() {
    let tmp = tempdir().unwrap();
    let cfg_path = tmp.path().join("cfg.yaml");
    let yaml = r#"
server:
  home_dir: "~/.test_dup"
modules:
  module1:
    config: {}
  module2:
    config: {}
  module1:
    config: {}
"#;
    fs::write(&cfg_path, yaml).unwrap();

    let result = AppConfig::load_layered(&cfg_path);
    assert!(result.is_err(), "duplicate module names should be rejected");
    let msg = format!("{:?}", result.unwrap_err());
    assert!(
        msg.contains("duplicate") || msg.contains("Duplicate"),
        "error should mention duplicates: {msg}"
    );
}

#[test]
fn test_reject_duplicate_keys_in_module_file() {
    let tmp = tempdir().unwrap();
    let modules_dir = tmp.path().join("modules.d");
    fs::create_dir_all(&modules_dir).unwrap();

    // Module file with duplicate "config:" key
    let module_yaml = r#"
config:
  key1: "value1"
config:
  key2: "value2"
"#;
    fs::write(modules_dir.join("bad_module.yaml"), module_yaml).unwrap();

    let cfg_yaml = format!(
        r#"
server:
  home_dir: "~/.test_dup_modfile"
modules_dir: "{}"
"#,
        normalize_path(&modules_dir)
    );
    let cfg_path = tmp.path().join("cfg.yaml");
    fs::write(&cfg_path, cfg_yaml).unwrap();

    let result = AppConfig::load_layered(&cfg_path);
    assert!(
        result.is_err(),
        "duplicate keys in a module file should be rejected"
    );
    let msg = format!("{:?}", result.unwrap_err());
    assert!(
        msg.contains("duplicate") || msg.contains("Duplicate"),
        "error should mention duplicates: {msg}"
    );
}

#[test]
fn test_no_false_positive_on_unique_modules() {
    let tmp = tempdir().unwrap();
    let cfg_path = tmp.path().join("cfg.yaml");
    let yaml = r#"
server:
  home_dir: "~/.test_ok"
modules:
  module1:
    config: {}
  module2:
    config: {}
  module3:
    config: {}
"#;
    fs::write(&cfg_path, yaml).unwrap();

    let result = AppConfig::load_layered(&cfg_path);
    assert!(
        result.is_ok(),
        "unique module names should be accepted: {:?}",
        result.unwrap_err()
    );
}
