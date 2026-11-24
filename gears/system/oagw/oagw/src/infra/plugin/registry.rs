use std::collections::HashMap;
use std::sync::Arc;

use crate::config::TokenCacheConfig;
use crate::domain::plugin::{AuthPlugin, GuardPlugin, PluginError, TransformPlugin};
use credstore_sdk::CredStoreClientV1;

use super::apikey_auth::ApiKeyAuthPlugin;
use super::aws_sigv4_auth::AwsSigV4AuthPlugin;
use super::noop_auth::NoopAuthPlugin;
use super::oauth2_client_cred_auth::{ClientAuthMethod, OAuth2ClientCredAuthPlugin};
use super::request_id_transform::RequestIdTransformPlugin;
use super::required_headers_guard::RequiredHeadersGuardPlugin;
use crate::domain::gts_helpers::{
    APIKEY_AUTH_PLUGIN_ID, AWS_SIGV4_AUTH_PLUGIN_ID, GUARD_PLUGIN_SCHEMA, NOOP_AUTH_PLUGIN_ID,
    OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID,
    OAUTH2_CLIENT_CRED_PRIVATE_KEY_JWT_AUTH_PLUGIN_ID, REQUEST_ID_TRANSFORM_PLUGIN_ID,
    REQUIRED_HEADERS_GUARD_PLUGIN_ID, TRANSFORM_PLUGIN_SCHEMA,
};

/// Registry that resolves auth plugin GTS identifiers to plugin implementations.
pub struct AuthPluginRegistry {
    plugins: HashMap<String, Arc<dyn AuthPlugin>>,
}

impl AuthPluginRegistry {
    /// Create a registry with the built-in plugins (apikey, noop, OAuth2
    /// client credentials with each client authentication method, AWS SigV4).
    #[must_use]
    pub fn with_builtins(
        credstore: Arc<dyn CredStoreClientV1>,
        token_http_config: Option<toolkit_http::HttpClientConfig>,
        token_cache_config: TokenCacheConfig,
    ) -> Self {
        let mut plugins: HashMap<String, Arc<dyn AuthPlugin>> = HashMap::new();
        plugins.insert(
            APIKEY_AUTH_PLUGIN_ID.to_string(),
            Arc::new(ApiKeyAuthPlugin::new(credstore.clone())),
        );
        plugins.insert(NOOP_AUTH_PLUGIN_ID.to_string(), Arc::new(NoopAuthPlugin));

        for (plugin_id, method) in [
            (OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, ClientAuthMethod::Form),
            (
                OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID,
                ClientAuthMethod::Basic,
            ),
            (
                OAUTH2_CLIENT_CRED_PRIVATE_KEY_JWT_AUTH_PLUGIN_ID,
                ClientAuthMethod::PrivateKeyJwt,
            ),
        ] {
            let mut plugin = OAuth2ClientCredAuthPlugin::new(
                credstore.clone(),
                method,
                token_cache_config.ttl,
                token_cache_config.capacity,
            );
            if let Some(ref cfg) = token_http_config {
                plugin = plugin.with_http_config(cfg.clone());
            }
            plugins.insert(plugin_id.to_string(), Arc::new(plugin));
        }
        plugins.insert(
            AWS_SIGV4_AUTH_PLUGIN_ID.to_string(),
            Arc::new(AwsSigV4AuthPlugin::new(credstore)),
        );
        Self { plugins }
    }

    /// Register `plugin` under `plugin_id`, replacing a built-in of that id,
    /// for tests of the pipeline.
    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) fn insert(&mut self, plugin_id: &str, plugin: Arc<dyn AuthPlugin>) {
        self.plugins.insert(plugin_id.to_owned(), plugin);
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

    /// Register `plugin` under `plugin_id`, for tests of the pipeline.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub(crate) fn with_plugin(mut self, plugin_id: &str, plugin: Arc<dyn TransformPlugin>) -> Self {
        self.plugins.insert(plugin_id.to_owned(), plugin);
        self
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

    /// Register `plugin` under `plugin_id`, for tests that need a transform
    /// the built-ins do not provide.
    #[cfg(test)]
    pub(crate) fn insert(&mut self, plugin_id: &str, plugin: Arc<dyn TransformPlugin>) {
        self.plugins.insert(plugin_id.to_owned(), plugin);
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
    use toolkit_gts::gts_id;

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
    fn resolves_aws_sigv4_plugin_which_signs() {
        let registry = make_registry();
        assert!(registry.resolve(AWS_SIGV4_AUTH_PLUGIN_ID).unwrap().signs());
    }

    #[test]
    fn resolves_oauth2_client_cred_private_key_jwt_plugin() {
        let registry = make_registry();
        assert!(
            registry
                .resolve(OAUTH2_CLIENT_CRED_PRIVATE_KEY_JWT_AUTH_PLUGIN_ID)
                .is_ok()
        );
    }

    #[test]
    fn unknown_plugin_returns_error() {
        let registry = make_registry();
        let err = registry.resolve(gts_id!(
            "cf.core.oagw.auth_plugin.v1~cf.core.oagw.unknown.v1"
        ));
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
        let err = registry.resolve(gts_id!(
            "cf.core.oagw.guard_plugin.v1~cf.core.oagw.unknown.v1"
        ));
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
        let err = registry.resolve(gts_id!(
            "cf.core.oagw.transform_plugin.v1~cf.core.oagw.unknown.v1"
        ));
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

#[cfg(test)]
mod implemented_ids_tests {
    use super::*;
    use crate::domain::gts_helpers::{
        IMPLEMENTED_AUTH_PLUGIN_IDS, IMPLEMENTED_GUARD_PLUGIN_IDS, IMPLEMENTED_TRANSFORM_PLUGIN_IDS,
    };

    fn sorted<'a>(ids: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        let mut ids: Vec<&str> = ids.into_iter().collect();
        ids.sort_unstable();
        ids
    }

    /// Write-time validation accepts exactly the plugins the registries run.
    #[test]
    fn implemented_ids_match_the_registries() {
        let auth = AuthPluginRegistry::with_builtins(
            Arc::new(credstore_sdk::test_util::MockCredStoreClient::empty()),
            None,
            TokenCacheConfig::default(),
        );
        assert_eq!(
            sorted(auth.plugins.keys().map(String::as_str)),
            sorted(IMPLEMENTED_AUTH_PLUGIN_IDS.iter().copied())
        );
        assert_eq!(
            sorted(
                GuardPluginRegistry::with_builtins()
                    .plugins
                    .keys()
                    .map(String::as_str)
            ),
            sorted(IMPLEMENTED_GUARD_PLUGIN_IDS.iter().copied())
        );
        assert_eq!(
            sorted(
                TransformPluginRegistry::with_builtins()
                    .plugins
                    .keys()
                    .map(String::as_str)
            ),
            sorted(IMPLEMENTED_TRANSFORM_PLUGIN_IDS.iter().copied())
        );
        // Every implemented auth plugin says which credential references
        // it reads, so writes check them.
        assert_eq!(
            sorted(
                crate::domain::plugin::credentials::AUTH_PLUGIN_CREDENTIAL_KEYS
                    .iter()
                    .map(|(plugin, _)| *plugin)
            ),
            sorted(IMPLEMENTED_AUTH_PLUGIN_IDS.iter().copied())
        );
        // Writes check `CREDENTIAL_KEYS`: it holds exactly the keys the
        // plugins read.
        let mut read: Vec<&str> = crate::domain::plugin::credentials::AUTH_PLUGIN_CREDENTIAL_KEYS
            .iter()
            .flat_map(|(_, keys)| keys.required.iter().chain(keys.optional).map(|k| k.key))
            .collect();
        read.sort_unstable();
        read.dedup();
        assert_eq!(
            read,
            sorted(
                crate::domain::plugin::credentials::CREDENTIAL_KEYS
                    .iter()
                    .map(|k| k.key)
            )
        );
    }

    /// Run `plugin` as the pipeline does: `authenticate`, then `sign` for a
    /// plugin that signs.
    async fn run(
        plugin: &dyn crate::domain::plugin::AuthPlugin,
        config: &std::collections::HashMap<String, String>,
        ctx: &toolkit_security::SecurityContext,
    ) -> Result<(), crate::domain::plugin::PluginError> {
        use crate::domain::plugin::{AuthContext, SigningBody, SigningContext};

        let mut auth_ctx = AuthContext::new(config.clone(), ctx.clone());
        plugin.authenticate(&mut auth_ctx).await?;
        if plugin.signs() {
            plugin
                .sign(&SigningContext {
                    method: "GET".into(),
                    scheme: "https".into(),
                    host: "api.example.com".into(),
                    port: 443,
                    path: "/".into(),
                    path_rewritten: false,
                    query: None,
                    headers: vec![("host".into(), b"api.example.com".to_vec())],
                    body: SigningBody::Buffered(bytes::Bytes::new()),
                    config: config.clone(),
                    security_context: ctx.clone(),
                })
                .await?;
        }
        Ok(())
    }

    /// Each implemented auth plugin needs exactly the required credential
    /// keys the table lists (without any one of them it refuses its config)
    /// and reads the optional ones when present.
    #[tokio::test]
    async fn credential_keys_are_the_ones_each_plugin_needs() {
        use crate::domain::plugin::PluginError;
        use crate::domain::plugin::credentials::{
            AUTH_PLUGIN_CREDENTIAL_KEYS, CredentialKey, PluginCredentialKeys,
        };
        use credstore_sdk::test_util::MockCredStoreClient;

        // Valid for every plugin: an access key id, a key long enough for
        // FIPS HMAC, a header value.
        const VALUE: &str = "Value0123456789";
        let ctx = toolkit_security::SecurityContext::builder()
            .subject_tenant_id(uuid::Uuid::new_v4())
            .subject_id(uuid::Uuid::new_v4())
            .build()
            .unwrap();
        let registry = |secrets: Vec<(String, String)>| {
            AuthPluginRegistry::with_builtins(
                Arc::new(MockCredStoreClient::with_secrets(secrets)),
                None,
                TokenCacheConfig::default(),
            )
        };
        let all = |keys: &PluginCredentialKeys| -> Vec<CredentialKey> {
            keys.required.iter().chain(keys.optional).copied().collect()
        };
        // The plugin's other config, and a distinct reference per key.
        let full = |keys: &[CredentialKey]| {
            let mut config: std::collections::HashMap<String, String> = [
                ("header", "authorization"),
                ("token_endpoint", "https://idp.example.com/token"),
                ("region", "us-east-1"),
                ("service", "bedrock"),
                ("signing_alg", "ES256"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
            for key in keys {
                config.insert(key.key.to_owned(), format!("cred://ref-{}", key.key));
            }
            config
        };
        let secrets = |keys: &[CredentialKey]| -> Vec<(String, String)> {
            keys.iter()
                .map(|k| (format!("ref-{}", k.key), VALUE.to_owned()))
                .collect()
        };
        for (plugin_id, keys) in AUTH_PLUGIN_CREDENTIAL_KEYS {
            let listed = all(keys);
            // With every listed key the config is complete: the plugin gets
            // as far as the (empty) credential store, or needs none at all.
            let plugin = registry(Vec::new()).resolve(plugin_id).unwrap();
            let complete = run(plugin.as_ref(), &full(&listed), &ctx).await;
            if listed.is_empty() {
                assert!(complete.is_ok(), "{plugin_id}: {complete:?}");
            } else {
                assert!(
                    matches!(complete, Err(PluginError::SecretNotFound { .. })),
                    "{plugin_id} with every listed key: {complete:?}"
                );
            }
            // Each listed key holds a credential reference: with a store
            // holding all but one, the lookup fails on that one's key, and
            // the error does not quote the ref.
            for absent in &listed {
                let others: Vec<CredentialKey> = listed
                    .iter()
                    .filter(|k| k.key != absent.key)
                    .copied()
                    .collect();
                let plugin = registry(secrets(&others)).resolve(plugin_id).unwrap();
                match run(plugin.as_ref(), &full(&listed), &ctx).await {
                    Err(e @ PluginError::SecretNotFound { key, .. }) => {
                        assert_eq!(key, absent.key, "{plugin_id}");
                        assert!(
                            !e.to_string().contains("ref-"),
                            "{plugin_id} without the secret of {}: {e}",
                            absent.key
                        );
                    }
                    other => panic!(
                        "{plugin_id} without the secret of {}: {other:?}",
                        absent.key
                    ),
                }
            }
            // Without a required key the plugin refuses its config and
            // names the key (the error kind is the plugin's own).
            for missing in keys.required {
                let mut config = full(&listed);
                config.remove(missing.key);
                match run(plugin.as_ref(), &config, &ctx).await {
                    Err(e) => assert!(
                        e.to_string().contains(missing.key),
                        "{plugin_id} without {}: {e}",
                        missing.key
                    ),
                    Ok(()) => panic!("{plugin_id} ran without {}", missing.key),
                }
            }
            // Without an optional key the plugin runs on the required ones.
            if !keys.optional.is_empty() {
                let plugin = registry(secrets(keys.required)).resolve(plugin_id).unwrap();
                let result = run(plugin.as_ref(), &full(keys.required), &ctx).await;
                assert!(result.is_ok(), "{plugin_id}: {result:?}");
            }
        }
    }
}
