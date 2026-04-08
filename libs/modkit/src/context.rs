use serde::de::DeserializeOwned;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// Import configuration types from the config module
use crate::config::{ConfigError, ConfigProvider, module_config_or_default};

// Note: runtime-dependent features are conditionally compiled

// DB types are available only when feature "db" is enabled.
// We keep local aliases so the rest of this file can compile without importing `modkit_db`.
#[cfg(feature = "db")]
pub(crate) type DbManager = modkit_db::DbManager;
#[cfg(feature = "db")]
pub(crate) type DbProvider = modkit_db::DBProvider<modkit_db::DbError>;

// Stub types for no-db builds (never exposed; methods that would use them are cfg'd out).
#[cfg(not(feature = "db"))]
#[derive(Clone, Debug)]
pub struct DbManager;
#[cfg(not(feature = "db"))]
#[derive(Clone, Debug)]
pub struct DbProvider;

#[derive(Clone)]
#[must_use]
pub struct ModuleCtx {
    module_name: Arc<str>,
    instance_id: Uuid,
    config_provider: Arc<dyn ConfigProvider>,
    client_hub: Arc<crate::client_hub::ClientHub>,
    cancellation_token: CancellationToken,
    db: Option<DbProvider>,
}

/// Builder for creating module-scoped contexts with resolved database handles.
///
/// This builder internally uses `DbManager` to resolve per-module `Db` instances
/// at build time, ensuring `ModuleCtx` contains only the final, ready-to-use entrypoint.
pub struct ModuleContextBuilder {
    instance_id: Uuid,
    config_provider: Arc<dyn ConfigProvider>,
    client_hub: Arc<crate::client_hub::ClientHub>,
    root_token: CancellationToken,
    db_manager: Option<Arc<DbManager>>, // internal only, never exposed to modules
}

impl ModuleContextBuilder {
    pub fn new(
        instance_id: Uuid,
        config_provider: Arc<dyn ConfigProvider>,
        client_hub: Arc<crate::client_hub::ClientHub>,
        root_token: CancellationToken,
        db_manager: Option<Arc<DbManager>>,
    ) -> Self {
        Self {
            instance_id,
            config_provider,
            client_hub,
            root_token,
            db_manager,
        }
    }

    /// Returns the process-level instance ID.
    #[must_use]
    pub fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    /// Build a module-scoped context, resolving the `DbHandle` for the given module.
    ///
    /// # Errors
    /// Returns an error if database resolution fails.
    pub async fn for_module(&self, module_name: &str) -> anyhow::Result<ModuleCtx> {
        let db: Option<DbProvider> = {
            #[cfg(feature = "db")]
            {
                if let Some(mgr) = &self.db_manager {
                    mgr.get(module_name).await?.map(modkit_db::DBProvider::new)
                } else {
                    None
                }
            }
            #[cfg(not(feature = "db"))]
            {
                let _ = module_name; // avoid unused in no-db builds
                None
            }
        };

        Ok(ModuleCtx::new(
            Arc::<str>::from(module_name),
            self.instance_id,
            self.config_provider.clone(),
            self.client_hub.clone(),
            self.root_token.child_token(),
            db,
        ))
    }
}

impl ModuleCtx {
    /// Create a new module-scoped context with all required fields.
    pub fn new(
        module_name: impl Into<Arc<str>>,
        instance_id: Uuid,
        config_provider: Arc<dyn ConfigProvider>,
        client_hub: Arc<crate::client_hub::ClientHub>,
        cancellation_token: CancellationToken,
        db: Option<DbProvider>,
    ) -> Self {
        Self {
            module_name: module_name.into(),
            instance_id,
            config_provider,
            client_hub,
            cancellation_token,
            db,
        }
    }

    // ---- public read-only API for modules ----

    #[inline]
    #[must_use]
    pub fn module_name(&self) -> &str {
        &self.module_name
    }

    /// Returns the process-level instance ID.
    ///
    /// This is a unique identifier for this process instance, shared by all modules
    /// in the same process. It is generated once at bootstrap.
    #[inline]
    #[must_use]
    pub fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    #[inline]
    #[must_use]
    pub fn config_provider(&self) -> &dyn ConfigProvider {
        &*self.config_provider
    }

    /// Get the `ClientHub` for dependency resolution.
    #[inline]
    #[must_use]
    pub fn client_hub(&self) -> Arc<crate::client_hub::ClientHub> {
        self.client_hub.clone()
    }

    #[inline]
    #[must_use]
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    /// Get a module-scoped DB entrypoint for secure database operations.
    ///
    /// Returns `None` if no database is configured for this module.
    ///
    /// # Security
    ///
    /// The returned `DBProvider<modkit_db::DbError>`:
    /// - Is cheap to clone (shares an internal `Db`)
    /// - Provides `conn()` for non-transactional access (fails inside tx via guard)
    /// - Provides `transaction(..)` for transactional operations
    ///
    /// # Example
    ///
    /// ```ignore
    /// let db = ctx.db().ok_or_else(|| anyhow!("no db"))?;
    /// let conn = db.conn()?;
    /// let user = svc.get_user(&conn, &scope, id).await?;
    /// ```
    #[must_use]
    #[cfg(feature = "db")]
    pub fn db(&self) -> Option<modkit_db::DBProvider<modkit_db::DbError>> {
        self.db.clone()
    }

    /// Get a database handle, returning an error if not configured.
    ///
    /// This is a convenience method that combines `db()` with an error for
    /// modules that require database access.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is not configured for this module.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let db = ctx.db_required()?;
    /// let conn = db.conn()?;
    /// let user = svc.get_user(&conn, &scope, id).await?;
    /// ```
    #[cfg(feature = "db")]
    pub fn db_required(&self) -> anyhow::Result<modkit_db::DBProvider<modkit_db::DbError>> {
        self.db().ok_or_else(|| {
            anyhow::anyhow!(
                "Database is not configured for module '{}'",
                self.module_name
            )
        })
    }

    /// Deserialize the module's config section into T, or use defaults if missing.
    ///
    /// This method uses lenient configuration loading: if the module is not present in config,
    /// has no config section, or the module entry is not an object, it returns `T::default()`.
    /// This allows modules to exist without configuration sections in the main config file.
    ///
    /// It extracts the 'config' field from: `modules.<name> = { database: ..., config: ... }`
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[derive(serde::Deserialize, Default)]
    /// struct MyConfig {
    ///     api_key: String,
    ///     timeout_ms: u64,
    /// }
    ///
    /// let config: MyConfig = ctx.config()?;
    /// ```
    ///
    /// # Errors
    /// Returns `ConfigError` if deserialization fails.
    pub fn config<T: DeserializeOwned + Default>(&self) -> Result<T, ConfigError> {
        module_config_or_default(self.config_provider.as_ref(), &self.module_name)
    }

    /// Like [`config()`](Self::config), but additionally expands `${VAR}` placeholders
    /// in fields marked with `#[expand_vars]` (requires `#[derive(ExpandVars)]` on the config
    /// struct).
    ///
    /// Modules that do not need environment variable expansion should use [`config()`](Self::config).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[derive(serde::Deserialize, Default, ExpandVars)]
    /// struct MyConfig {
    ///     #[expand_vars]
    ///     api_key: String,
    ///     timeout_ms: u64,
    /// }
    ///
    /// let config: MyConfig = ctx.config_expanded()?;
    /// ```
    ///
    /// # Errors
    /// Returns `ConfigError` if deserialization fails or if environment variable expansion fails.
    pub fn config_expanded<T>(&self) -> Result<T, ConfigError>
    where
        T: DeserializeOwned + Default + crate::var_expand::ExpandVars,
    {
        let mut cfg: T = self.config()?;
        cfg.expand_vars().map_err(|e| ConfigError::VarExpand {
            module: self.module_name.to_string(),
            source: e,
        })?;
        Ok(cfg)
    }

    /// Get the raw JSON value of the module's config section.
    /// Returns the 'config' field from: modules.<name> = { database: ..., config: ... }
    #[must_use]
    pub fn raw_config(&self) -> &serde_json::Value {
        use std::sync::LazyLock;

        static EMPTY: LazyLock<serde_json::Value> =
            LazyLock::new(|| serde_json::Value::Object(serde_json::Map::new()));

        if let Some(module_raw) = self.config_provider.get_module_config(&self.module_name) {
            // Try new structure first: modules.<name> = { database: ..., config: ... }
            if let Some(obj) = module_raw.as_object()
                && let Some(config_section) = obj.get("config")
            {
                return config_section;
            }
        }
        &EMPTY
    }

    /// Create a derivative context with the same references but no DB handle.
    /// Useful for modules that don't require database access.
    pub fn without_db(&self) -> ModuleCtx {
        ModuleCtx {
            module_name: self.module_name.clone(),
            instance_id: self.instance_id,
            config_provider: self.config_provider.clone(),
            client_hub: self.client_hub.clone(),
            cancellation_token: self.cancellation_token.clone(),
            db: None,
        }
    }
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
}
