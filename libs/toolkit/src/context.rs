use serde::de::DeserializeOwned;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// Import configuration types from the config gear
use crate::{
    config::{ConfigError, ConfigProvider, gear_config_or_default},
    gear_config_required,
};

#[cfg(feature = "db")]
pub(crate) type DbManager = toolkit_db::DbManager;
#[cfg(feature = "db")]
pub(crate) type DbProvider = toolkit_db::DBProvider<toolkit_db::DbError>;

#[derive(Clone)]
#[must_use]
pub struct GearCtx {
    gear_name: Arc<str>,
    instance_id: Uuid,
    config_provider: Arc<dyn ConfigProvider>,
    client_hub: Arc<crate::client_hub::ClientHub>,
    cancellation_token: CancellationToken,
    #[cfg(feature = "db")]
    db: Option<DbProvider>,
}

/// Builder for creating gear-scoped contexts with resolved database handles.
///
/// Use [`GearContextBuilder::with_db_manager`] (feature `db`) to attach a
/// `DbManager` so `for_gear` can resolve a per-gear `DbHandle`.
#[must_use]
pub struct GearContextBuilder {
    instance_id: Uuid,
    config_provider: Arc<dyn ConfigProvider>,
    client_hub: Arc<crate::client_hub::ClientHub>,
    root_token: CancellationToken,
    #[cfg(feature = "db")]
    db_manager: Option<Arc<DbManager>>, // internal only, never exposed to gears
}

impl GearContextBuilder {
    pub fn new(
        instance_id: Uuid,
        config_provider: Arc<dyn ConfigProvider>,
        client_hub: Arc<crate::client_hub::ClientHub>,
        root_token: CancellationToken,
    ) -> Self {
        Self {
            instance_id,
            config_provider,
            client_hub,
            root_token,
            #[cfg(feature = "db")]
            db_manager: None,
        }
    }

    /// Attach a `DbManager` used by [`for_gear`](Self::for_gear) to resolve
    /// per-gear database handles.
    #[cfg(feature = "db")]
    pub fn with_db_manager(mut self, db_manager: Arc<DbManager>) -> Self {
        self.db_manager = Some(db_manager);
        self
    }

    /// Returns the process-level instance ID.
    #[must_use]
    pub fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    /// Build a gear-scoped context, resolving the `DbHandle` for the given
    /// gear when the `db` feature is enabled.
    ///
    /// Kept `async` in both configurations so callers don't need cfg branches
    /// around `.await`; under `not(feature = "db")` the future is ready on
    /// first poll.
    ///
    /// # Errors
    /// Returns an error if database resolution fails.
    #[cfg_attr(not(feature = "db"), allow(clippy::unused_async))]
    pub async fn for_gear(&self, gear_name: &str) -> anyhow::Result<GearCtx> {
        let ctx = GearCtx::new(
            Arc::<str>::from(gear_name),
            self.instance_id,
            self.config_provider.clone(),
            self.client_hub.clone(),
            self.root_token.child_token(),
        );
        #[cfg(feature = "db")]
        let ctx = if let Some(mgr) = &self.db_manager
            && let Some(handle) = mgr.get(gear_name).await?
        {
            ctx.with_db(toolkit_db::DBProvider::new(handle))
        } else {
            ctx
        };
        Ok(ctx)
    }
}

impl GearCtx {
    /// Create a new gear-scoped context with all required fields.
    ///
    /// Attach a database entrypoint with [`with_db`](Self::with_db) (feature `db`).
    pub fn new(
        gear_name: impl Into<Arc<str>>,
        instance_id: Uuid,
        config_provider: Arc<dyn ConfigProvider>,
        client_hub: Arc<crate::client_hub::ClientHub>,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            gear_name: gear_name.into(),
            instance_id,
            config_provider,
            client_hub,
            cancellation_token,
            #[cfg(feature = "db")]
            db: None,
        }
    }

    /// Attach the per-gear database entrypoint.
    #[cfg(feature = "db")]
    pub fn with_db(mut self, db: DbProvider) -> Self {
        self.db = Some(db);
        self
    }

    // ---- public read-only API for gears ----

    #[inline]
    #[must_use]
    pub fn gear_name(&self) -> &str {
        &self.gear_name
    }

    /// Returns the process-level instance ID.
    ///
    /// This is a unique identifier for this process instance, shared by all gears
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

    /// Get a gear-scoped DB entrypoint for secure database operations.
    ///
    /// Returns `None` if no database is configured for this gear.
    ///
    /// # Security
    ///
    /// The returned `DBProvider<toolkit_db::DbError>`:
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
    pub fn db(&self) -> Option<toolkit_db::DBProvider<toolkit_db::DbError>> {
        self.db.clone()
    }

    /// Get a database handle, returning an error if not configured.
    ///
    /// This is a convenience method that combines `db()` with an error for
    /// gears that require database access.
    ///
    /// # Errors
    ///
    /// Returns an error if the database is not configured for this gear.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let db = ctx.db_required()?;
    /// let conn = db.conn()?;
    /// let user = svc.get_user(&conn, &scope, id).await?;
    /// ```
    #[cfg(feature = "db")]
    pub fn db_required(&self) -> anyhow::Result<toolkit_db::DBProvider<toolkit_db::DbError>> {
        self.db().ok_or_else(|| {
            anyhow::anyhow!("Database is not configured for gear '{}'", self.gear_name)
        })
    }

    /// Deserialize the gear's config section into `T`.
    ///
    /// This reads the `gears.<name>.config` object for the current gear and
    /// deserializes it into the requested type.
    ///
    /// # Errors
    /// Returns `ConfigError` if the gear config is missing or deserialization fails.
    pub fn config<T: DeserializeOwned>(&self) -> Result<T, ConfigError> {
        gear_config_required(self.config_provider.as_ref(), &self.gear_name)
    }

    /// Deserialize the gear's config section into T, or use defaults if missing.
    ///
    /// This method uses lenient configuration loading: if the gear is not present in config,
    /// has no config section, or the gear entry is not an object, it returns `T::default()`.
    /// This allows gears to exist without configuration sections in the main config file.
    ///
    /// It extracts the 'config' field from: `gears.<name> = { database: ..., config: ... }`
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
    /// let config: MyConfig = ctx.config_or_default()?;
    /// ```
    ///
    /// # Errors
    /// Returns `ConfigError` if deserialization fails.
    pub fn config_or_default<T: DeserializeOwned + Default>(&self) -> Result<T, ConfigError> {
        gear_config_or_default(self.config_provider.as_ref(), &self.gear_name)
    }

    /// Like [`config()`](Self::config), but additionally expands `${VAR}` placeholders
    /// in fields marked with `#[expand_vars]`.
    ///
    /// # Errors
    /// Returns `ConfigError` if the gear config is missing, deserialization fails,
    /// or environment variable expansion fails.
    pub fn config_expanded<T>(&self) -> Result<T, ConfigError>
    where
        T: DeserializeOwned + crate::var_expand::ExpandVars,
    {
        let mut cfg: T = self.config()?;
        cfg.expand_vars().map_err(|e| ConfigError::VarExpand {
            gear: self.gear_name.to_string(),
            cause: e,
        })?;
        Ok(cfg)
    }

    /// Like [`config_or_default()`](Self::config_or_default), but additionally expands `${VAR}`
    /// placeholders
    /// in fields marked with `#[expand_vars]` (requires `#[derive(ExpandVars)]` on the config
    /// struct).
    ///
    /// Gears that do not need environment variable expansion should use
    /// [`config_or_default()`](Self::config_or_default).
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
    /// let config: MyConfig = ctx.config_expanded_or_default()?;
    /// ```
    ///
    /// # Errors
    /// Returns `ConfigError` if deserialization fails or if environment variable expansion fails.
    pub fn config_expanded_or_default<T>(&self) -> Result<T, ConfigError>
    where
        T: DeserializeOwned + Default + crate::var_expand::ExpandVars,
    {
        let mut cfg: T = self.config_or_default()?;
        cfg.expand_vars().map_err(|e| ConfigError::VarExpand {
            gear: self.gear_name.to_string(),
            cause: e,
        })?;
        Ok(cfg)
    }

    /// Get the raw JSON value of the gear's config section.
    /// Returns the 'config' field from: gears.<name> = { database: ..., config: ... }
    #[must_use]
    pub fn raw_config(&self) -> &serde_json::Value {
        use std::sync::LazyLock;

        static EMPTY: LazyLock<serde_json::Value> =
            LazyLock::new(|| serde_json::Value::Object(serde_json::Map::new()));

        if let Some(gear_raw) = self.config_provider.get_gear_config(&self.gear_name) {
            // Try new structure first: gears.<name> = { database: ..., config: ... }
            if let Some(obj) = gear_raw.as_object()
                && let Some(config_section) = obj.get("config")
            {
                return config_section;
            }
        }
        &EMPTY
    }

    /// Create a derivative context with the same references but no DB handle.
    /// Useful for gears that don't require database access.
    pub fn without_db(&self) -> GearCtx {
        GearCtx {
            gear_name: self.gear_name.clone(),
            instance_id: self.instance_id,
            config_provider: self.config_provider.clone(),
            client_hub: self.client_hub.clone(),
            cancellation_token: self.cancellation_token.clone(),
            #[cfg(feature = "db")]
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

            Self { gears }
        }
    }

    impl ConfigProvider for MockConfigProvider {
        fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
            self.gears.get(gear_name)
        }
    }

    #[test]
    fn test_gear_ctx_config_with_valid_config() {
        let provider = Arc::new(MockConfigProvider::new());
        let ctx = GearCtx::new(
            "test_gear",
            Uuid::new_v4(),
            provider,
            Arc::new(crate::client_hub::ClientHub::default()),
            CancellationToken::new(),
        );

        let result: Result<TestConfig, ConfigError> = ctx.config();
        assert!(result.is_ok());

        let config = result.unwrap();
        assert_eq!(config.api_key, "secret123");
        assert_eq!(config.timeout_ms, 5000);
        assert!(config.enabled);
    }

    #[test]
    fn test_gear_ctx_config_returns_error_for_missing_gear() {
        let provider = Arc::new(MockConfigProvider::new());
        let ctx = GearCtx::new(
            "nonexistent_gear",
            Uuid::new_v4(),
            provider,
            Arc::new(crate::client_hub::ClientHub::default()),
            CancellationToken::new(),
        );

        let result: Result<TestConfig, ConfigError> = ctx.config();
        assert!(matches!(
            result,
            Err(ConfigError::GearNotFound { ref gear }) if gear == "nonexistent_gear"
        ));
    }

    #[test]
    fn test_gear_ctx_config_or_default_returns_default_for_missing_gear() {
        let provider = Arc::new(MockConfigProvider::new());
        let ctx = GearCtx::new(
            "nonexistent_gear",
            Uuid::new_v4(),
            provider,
            Arc::new(crate::client_hub::ClientHub::default()),
            CancellationToken::new(),
        );

        let result: Result<TestConfig, ConfigError> = ctx.config_or_default();
        assert!(result.is_ok());

        let config = result.unwrap();
        assert_eq!(config, TestConfig::default());
    }

    #[test]
    fn test_gear_ctx_instance_id() {
        let provider = Arc::new(MockConfigProvider::new());
        let instance_id = Uuid::new_v4();
        let ctx = GearCtx::new(
            "test_gear",
            instance_id,
            provider,
            Arc::new(crate::client_hub::ClientHub::default()),
            CancellationToken::new(),
        );

        assert_eq!(ctx.instance_id(), instance_id);
    }

    // --- config_expanded tests ---

    #[derive(Debug, PartialEq, Deserialize, Default, toolkit_macros::ExpandVars)]
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

    fn make_ctx(gear_name: &str, config_json: serde_json::Value) -> GearCtx {
        let mut gears = HashMap::new();
        gears.insert(gear_name.to_owned(), config_json);
        let provider = Arc::new(MockConfigProvider { gears });
        GearCtx::new(
            gear_name,
            Uuid::new_v4(),
            provider,
            Arc::new(crate::client_hub::ClientHub::default()),
            CancellationToken::new(),
        )
    }

    #[test]
    fn config_expanded_resolves_env_vars() {
        let ctx = make_ctx(
            "expand_mod",
            json!({
                "config": {
                    "api_key": "${TOOLKIT_TEST_KEY}",
                    "endpoint": "https://${TOOLKIT_TEST_HOST}/api",
                    "retries": 3
                }
            }),
        );

        temp_env::with_vars(
            [
                ("TOOLKIT_TEST_KEY", Some("secret-42")),
                ("TOOLKIT_TEST_HOST", Some("example.com")),
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
                    "api_key": "${TOOLKIT_TEST_MISSING_VAR_XYZ}"
                }
            }),
        );

        temp_env::with_vars([("TOOLKIT_TEST_MISSING_VAR_XYZ", None::<&str>)], || {
            let err = ctx.config_expanded::<ExpandableConfig>().unwrap_err();
            assert!(
                matches!(err, ConfigError::VarExpand { ref gear, .. } if gear == "expand_mod"),
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
    fn config_expanded_returns_error_when_missing() {
        let ctx = make_ctx("missing_mod", json!({}));
        let err = ctx.config_expanded::<ExpandableConfig>().unwrap_err();
        assert!(matches!(
            err,
            ConfigError::MissingConfigSection { ref gear } if gear == "missing_mod"
        ));
    }

    #[test]
    fn config_expanded_or_default_falls_back_to_default_when_missing() {
        let ctx = make_ctx("missing_mod", json!({}));
        let cfg: ExpandableConfig = ctx.config_expanded_or_default().unwrap();
        assert_eq!(cfg, ExpandableConfig::default());
    }

    // --- nested struct expansion ---

    #[derive(Debug, PartialEq, Deserialize, Default, toolkit_macros::ExpandVars)]
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

    #[derive(Debug, PartialEq, Deserialize, Default, toolkit_macros::ExpandVars)]
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
                    "name": "${TOOLKIT_NESTED_NAME}",
                    "providers": {
                        "primary": {
                            "host": "${TOOLKIT_NESTED_HOST}",
                            "token": "${TOOLKIT_NESTED_TOKEN}",
                            "auth_config": {
                                "header": "X-Api-Key",
                                "secret_ref": "${TOOLKIT_NESTED_SECRET}"
                            },
                            "port": 443
                        }
                    },
                    "tags": ["${TOOLKIT_NESTED_TAG}", "literal"]
                }
            }),
        );

        temp_env::with_vars(
            [
                ("TOOLKIT_NESTED_NAME", Some("my-service")),
                ("TOOLKIT_NESTED_HOST", Some("api.example.com")),
                ("TOOLKIT_NESTED_TOKEN", Some("sk-secret")),
                ("TOOLKIT_NESTED_SECRET", Some("key-12345")),
                ("TOOLKIT_NESTED_TAG", Some("production")),
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
                        "bad": { "host": "${TOOLKIT_NESTED_GONE}", "port": 80 }
                    }
                }
            }),
        );

        temp_env::with_vars([("TOOLKIT_NESTED_GONE", None::<&str>)], || {
            let err = ctx.config_expanded::<NestedConfig>().unwrap_err();
            assert!(
                matches!(err, ConfigError::VarExpand { ref gear, .. } if gear == "nested_mod"),
                "expected EnvExpand, got: {err:?}"
            );
        });
    }
}
