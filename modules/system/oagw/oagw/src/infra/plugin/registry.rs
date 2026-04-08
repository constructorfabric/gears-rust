use std::collections::HashMap;
use std::sync::Arc;

use modkit_auth::oauth2::types::ClientAuthMethod;

use crate::config::TokenCacheConfig;
use crate::domain::plugin::{AuthPlugin, GuardPlugin, PluginError, TransformPlugin};
use credstore_sdk::CredStoreClientV1;

use super::apikey_auth::ApiKeyAuthPlugin;
use super::noop_auth::NoopAuthPlugin;
use super::oauth2_client_cred_auth::OAuth2ClientCredAuthPlugin;
use super::request_id_transform::RequestIdTransformPlugin;
use super::required_headers_guard::RequiredHeadersGuardPlugin;
use crate::domain::gts_helpers::{
    APIKEY_AUTH_PLUGIN_ID, GUARD_PLUGIN_SCHEMA, NOOP_AUTH_PLUGIN_ID,
    OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID,
    REQUEST_ID_TRANSFORM_PLUGIN_ID, REQUIRED_HEADERS_GUARD_PLUGIN_ID, TRANSFORM_PLUGIN_SCHEMA,
};

/// Registry that resolves auth plugin GTS identifiers to plugin implementations.
pub struct AuthPluginRegistry {
    plugins: HashMap<String, Arc<dyn AuthPlugin>>,
}

impl AuthPluginRegistry {
    /// Create a registry with the built-in plugins (apikey, noop, oauth2 CC).
    #[must_use]
    pub fn with_builtins(
        credstore: Arc<dyn CredStoreClientV1>,
        token_http_config: Option<modkit_http::HttpClientConfig>,
        token_cache_config: TokenCacheConfig,
    ) -> Self {
        let mut plugins: HashMap<String, Arc<dyn AuthPlugin>> = HashMap::new();
        plugins.insert(
            APIKEY_AUTH_PLUGIN_ID.to_string(),
            Arc::new(ApiKeyAuthPlugin::new(credstore.clone())),
        );
        plugins.insert(NOOP_AUTH_PLUGIN_ID.to_string(), Arc::new(NoopAuthPlugin));

        let mut form_plugin = OAuth2ClientCredAuthPlugin::new(
            credstore.clone(),
            ClientAuthMethod::Form,
            token_cache_config.ttl,
            token_cache_config.capacity,
        );
        let mut basic_plugin = OAuth2ClientCredAuthPlugin::new(
            credstore.clone(),
            ClientAuthMethod::Basic,
            token_cache_config.ttl,
            token_cache_config.capacity,
        );
        if let Some(ref cfg) = token_http_config {
            form_plugin = form_plugin.with_http_config(cfg.clone());
            basic_plugin = basic_plugin.with_http_config(cfg.clone());
        }

        plugins.insert(
            OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.to_string(),
            Arc::new(form_plugin),
        );
        plugins.insert(
            OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID.to_string(),
            Arc::new(basic_plugin),
        );
        Self { plugins }
    }

    /// Resolve a plugin by its GTS identifier.
    ///
    /// # Errors
    /// Returns `PluginError::Internal` if the plugin is not registered.
    pub fn resolve(&self, plugin_id: &str) -> Result<Arc<dyn AuthPlugin>, PluginError> {
        self.plugins
            .get(plugin_id)
            .cloned()
            .ok_or_else(|| PluginError::Internal(format!("unknown auth plugin: {plugin_id}")))
    }
}

// ---------------------------------------------------------------------------
// Guard plugin registry
// ---------------------------------------------------------------------------

/// Registry that resolves guard plugin GTS identifiers to plugin implementations.
pub struct GuardPluginRegistry {
    plugins: HashMap<String, Arc<dyn GuardPlugin>>,
}

impl GuardPluginRegistry {
    /// Create a registry with the built-in guard plugins.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut plugins: HashMap<String, Arc<dyn GuardPlugin>> = HashMap::new();
        plugins.insert(
            REQUIRED_HEADERS_GUARD_PLUGIN_ID.to_string(),
            Arc::new(RequiredHeadersGuardPlugin),
        );
        Self { plugins }
    }

    /// Resolve a guard plugin by its GTS identifier.
    ///
    /// # Errors
    /// Returns `PluginError::Internal` if the plugin is not registered.
    pub fn resolve(&self, plugin_id: &str) -> Result<Arc<dyn GuardPlugin>, PluginError> {
        self.plugins
            .get(plugin_id)
            .cloned()
            .ok_or_else(|| PluginError::Internal(format!("unknown guard plugin: {plugin_id}")))
    }

    /// Check if a GTS identifier belongs to the guard plugin schema.
    #[must_use]
    pub fn is_guard_plugin(gts_id: &str) -> bool {
        gts_id.starts_with(GUARD_PLUGIN_SCHEMA)
    }
}

// ---------------------------------------------------------------------------
// Transform plugin registry
// ---------------------------------------------------------------------------

/// Registry that resolves transform plugin GTS identifiers to plugin implementations.
pub struct TransformPluginRegistry {
    plugins: HashMap<String, Arc<dyn TransformPlugin>>,
}

impl TransformPluginRegistry {
    /// Create a registry with the built-in transform plugins.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut plugins: HashMap<String, Arc<dyn TransformPlugin>> = HashMap::new();
        plugins.insert(
            REQUEST_ID_TRANSFORM_PLUGIN_ID.to_string(),
            Arc::new(RequestIdTransformPlugin),
        );
        Self { plugins }
    }

    /// Resolve a transform plugin by its GTS identifier.
    ///
    /// # Errors
    /// Returns `PluginError::Internal` if the plugin is not registered.
    pub fn resolve(&self, plugin_id: &str) -> Result<Arc<dyn TransformPlugin>, PluginError> {
        self.plugins
            .get(plugin_id)
            .cloned()
            .ok_or_else(|| PluginError::Internal(format!("unknown transform plugin: {plugin_id}")))
    }

    /// Check if a GTS identifier belongs to the transform plugin schema.
    #[must_use]
    pub fn is_transform_plugin(gts_id: &str) -> bool {
        gts_id.starts_with(TRANSFORM_PLUGIN_SCHEMA)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::domain::test_support::MockCredStoreClient;

    use super::*;

    fn make_registry() -> AuthPluginRegistry {
        AuthPluginRegistry::with_builtins(
            Arc::new(MockCredStoreClient::empty()),
            None,
            TokenCacheConfig::default(),
        )
    }

    #[test]
    fn resolves_apikey_plugin() {
        let registry = make_registry();
        assert!(registry.resolve(APIKEY_AUTH_PLUGIN_ID).is_ok());
    }

    #[test]
    fn resolves_noop_plugin() {
        let registry = make_registry();
        assert!(registry.resolve(NOOP_AUTH_PLUGIN_ID).is_ok());
    }

    #[test]
    fn resolves_oauth2_client_cred_form_plugin() {
        let registry = make_registry();
        assert!(registry.resolve(OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID).is_ok());
    }

    #[test]
    fn resolves_oauth2_client_cred_basic_plugin() {
        let registry = make_registry();
        assert!(
            registry
                .resolve(OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID)
                .is_ok()
        );
    }

    #[test]
    fn unknown_plugin_returns_error() {
        let registry = make_registry();
        let err = registry.resolve("gts.x.core.oagw.auth_plugin.v1~x.core.oagw.unknown.v1");
        assert!(err.is_err());
    }

    #[test]
    fn resolves_required_headers_guard_plugin() {
        let registry = GuardPluginRegistry::with_builtins();
        assert!(registry.resolve(REQUIRED_HEADERS_GUARD_PLUGIN_ID).is_ok());
    }

    #[test]
    fn unknown_guard_plugin_returns_error() {
        let registry = GuardPluginRegistry::with_builtins();
        let err = registry.resolve("gts.x.core.oagw.guard_plugin.v1~x.core.oagw.unknown.v1");
        assert!(err.is_err());
    }

    #[test]
    fn is_guard_plugin_matches_guard_schema() {
        assert!(GuardPluginRegistry::is_guard_plugin(
            REQUIRED_HEADERS_GUARD_PLUGIN_ID
        ));
        assert!(!GuardPluginRegistry::is_guard_plugin(APIKEY_AUTH_PLUGIN_ID));
    }

    #[test]
    fn resolves_request_id_transform_plugin() {
        let registry = TransformPluginRegistry::with_builtins();
        assert!(registry.resolve(REQUEST_ID_TRANSFORM_PLUGIN_ID).is_ok());
    }

    #[test]
    fn unknown_transform_plugin_returns_error() {
        let registry = TransformPluginRegistry::with_builtins();
        let err = registry.resolve("gts.x.core.oagw.transform_plugin.v1~x.core.oagw.unknown.v1");
        assert!(err.is_err());
    }

    #[test]
    fn is_transform_plugin_matches_transform_schema() {
        assert!(TransformPluginRegistry::is_transform_plugin(
            REQUEST_ID_TRANSFORM_PLUGIN_ID
        ));
        assert!(!TransformPluginRegistry::is_transform_plugin(
            APIKEY_AUTH_PLUGIN_ID
        ));
    }
}
