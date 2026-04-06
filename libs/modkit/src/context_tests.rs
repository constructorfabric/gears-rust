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
    modules: HashMap<String, serde_json::Value>,
}

impl MockConfigProvider {
    fn new() -> Self {
        let mut modules = HashMap::new();

        // Valid module config
        modules.insert(
            "test_module".to_owned(),
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

        Self { modules }
    }
}

impl ConfigProvider for MockConfigProvider {
    fn get_module_config(&self, module_name: &str) -> Option<&serde_json::Value> {
        self.modules.get(module_name)
    }
}

#[test]
fn test_module_ctx_config_with_valid_config() {
    let provider = Arc::new(MockConfigProvider::new());
    let ctx = ModuleCtx::new(
        "test_module",
        Uuid::new_v4(),
        provider,
        Arc::new(crate::client_hub::ClientHub::default()),
        CancellationToken::new(),
        None,
    );

    let result: Result<TestConfig, ConfigError> = ctx.config();
    assert!(result.is_ok());

    let config = result.unwrap();
    assert_eq!(config.api_key, "secret123");
    assert_eq!(config.timeout_ms, 5000);
    assert!(config.enabled);
}

#[test]
fn test_module_ctx_config_returns_default_for_missing_module() {
    let provider = Arc::new(MockConfigProvider::new());
    let ctx = ModuleCtx::new(
        "nonexistent_module",
        Uuid::new_v4(),
        provider,
        Arc::new(crate::client_hub::ClientHub::default()),
        CancellationToken::new(),
        None,
    );

    let result: Result<TestConfig, ConfigError> = ctx.config();
    assert!(result.is_ok());

    let config = result.unwrap();
    assert_eq!(config, TestConfig::default());
}

#[test]
fn test_module_ctx_instance_id() {
    let provider = Arc::new(MockConfigProvider::new());
    let instance_id = Uuid::new_v4();
    let ctx = ModuleCtx::new(
        "test_module",
        instance_id,
        provider,
        Arc::new(crate::client_hub::ClientHub::default()),
        CancellationToken::new(),
        None,
    );

    assert_eq!(ctx.instance_id(), instance_id);
}

// --- config_expanded tests ---

#[derive(Debug, PartialEq, Deserialize, Default, modkit_macros::ExpandVars)]
struct ExpandableConfig {
    #[expand_vars]
    #[serde(default)]
    api_key: String,
    #[expand_vars]
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    retries: u32,
}

fn make_ctx(module_name: &str, config_json: serde_json::Value) -> ModuleCtx {
    let mut modules = HashMap::new();
    modules.insert(module_name.to_owned(), config_json);
    let provider = Arc::new(MockConfigProvider { modules });
    ModuleCtx::new(
        module_name,
        Uuid::new_v4(),
        provider,
        Arc::new(crate::client_hub::ClientHub::default()),
        CancellationToken::new(),
        None,
    )
}

#[test]
fn config_expanded_resolves_env_vars() {
    let ctx = make_ctx(
        "expand_mod",
        json!({
            "config": {
                "api_key": "${MODKIT_TEST_KEY}",
                "endpoint": "https://${MODKIT_TEST_HOST}/api",
                "retries": 3
            }
        }),
    );

    temp_env::with_vars(
        [
            ("MODKIT_TEST_KEY", Some("secret-42")),
            ("MODKIT_TEST_HOST", Some("example.com")),
        ],
        || {
            let cfg: ExpandableConfig = ctx.config_expanded().unwrap();
            assert_eq!(cfg.api_key, "secret-42");
            assert_eq!(cfg.endpoint.as_deref(), Some("https://example.com/api"));
            assert_eq!(cfg.retries, 3);
        },
    );
}

#[test]
fn config_expanded_returns_error_on_missing_var() {
    let ctx = make_ctx(
        "expand_mod",
        json!({
            "config": {
                "api_key": "${MODKIT_TEST_MISSING_VAR_XYZ}"
            }
        }),
    );

    temp_env::with_vars([("MODKIT_TEST_MISSING_VAR_XYZ", None::<&str>)], || {
        let err = ctx.config_expanded::<ExpandableConfig>().unwrap_err();
        assert!(
            matches!(err, ConfigError::VarExpand { ref module, .. } if module == "expand_mod"),
            "expected EnvExpand error, got: {err:?}"
        );
    });
}

#[test]
fn config_expanded_skips_none_option_fields() {
    let ctx = make_ctx(
        "expand_mod",
        json!({
            "config": {
                "api_key": "literal-key",
                "retries": 5
            }
        }),
    );

    let cfg: ExpandableConfig = ctx.config_expanded().unwrap();
    assert_eq!(cfg.api_key, "literal-key");
    assert_eq!(cfg.endpoint, None);
    assert_eq!(cfg.retries, 5);
}

#[test]
fn config_expanded_falls_back_to_default_when_missing() {
    let ctx = make_ctx("missing_mod", json!({}));
    let cfg: ExpandableConfig = ctx.config_expanded().unwrap();
    assert_eq!(cfg, ExpandableConfig::default());
}

// --- nested struct expansion ---

#[derive(Debug, PartialEq, Deserialize, Default, modkit_macros::ExpandVars)]
struct NestedProvider {
    #[expand_vars]
    #[serde(default)]
    host: String,
    #[expand_vars]
    #[serde(default)]
    token: Option<String>,
    #[expand_vars]
    #[serde(default)]
    auth_config: Option<HashMap<String, String>>,
    #[serde(default)]
    port: u16,
}

#[derive(Debug, PartialEq, Deserialize, Default, modkit_macros::ExpandVars)]
struct NestedConfig {
    #[expand_vars]
    #[serde(default)]
    name: String,
    #[expand_vars]
    #[serde(default)]
    providers: HashMap<String, NestedProvider>,
    #[expand_vars]
    #[serde(default)]
    tags: Vec<String>,
}

#[test]
fn config_expanded_resolves_nested_structs() {
    let ctx = make_ctx(
        "nested_mod",
        json!({
            "config": {
                "name": "${MODKIT_NESTED_NAME}",
                "providers": {
                    "primary": {
                        "host": "${MODKIT_NESTED_HOST}",
                        "token": "${MODKIT_NESTED_TOKEN}",
                        "auth_config": {
                            "header": "X-Api-Key",
                            "secret_ref": "${MODKIT_NESTED_SECRET}"
                        },
                        "port": 443
                    }
                },
                "tags": ["${MODKIT_NESTED_TAG}", "literal"]
            }
        }),
    );

    temp_env::with_vars(
        [
            ("MODKIT_NESTED_NAME", Some("my-service")),
            ("MODKIT_NESTED_HOST", Some("api.example.com")),
            ("MODKIT_NESTED_TOKEN", Some("sk-secret")),
            ("MODKIT_NESTED_SECRET", Some("key-12345")),
            ("MODKIT_NESTED_TAG", Some("production")),
        ],
        || {
            let cfg: NestedConfig = ctx.config_expanded().unwrap();
            assert_eq!(cfg.name, "my-service");
            assert_eq!(cfg.tags, vec!["production", "literal"]);

            let primary = cfg.providers.get("primary").expect("primary provider");
            assert_eq!(primary.host, "api.example.com");
            assert_eq!(primary.token.as_deref(), Some("sk-secret"));
            assert_eq!(primary.port, 443);

            let auth = primary.auth_config.as_ref().expect("auth_config present");
            assert_eq!(auth.get("header").map(String::as_str), Some("X-Api-Key"));
            assert_eq!(
                auth.get("secret_ref").map(String::as_str),
                Some("key-12345")
            );
        },
    );
}

#[test]
fn config_expanded_nested_missing_var_returns_error() {
    let ctx = make_ctx(
        "nested_mod",
        json!({
            "config": {
                "name": "ok",
                "providers": {
                    "bad": { "host": "${MODKIT_NESTED_GONE}", "port": 80 }
                }
            }
        }),
    );

    temp_env::with_vars([("MODKIT_NESTED_GONE", None::<&str>)], || {
        let err = ctx.config_expanded::<NestedConfig>().unwrap_err();
        assert!(
            matches!(err, ConfigError::VarExpand { ref module, .. } if module == "nested_mod"),
            "expected EnvExpand, got: {err:?}"
        );
    });
}
