use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::{CredStoreClientV1, SecretRef};
use serde::Deserialize;

use crate::domain::plugin::credentials::{SECRET_REF_KEY, credstore_error_kind};
use crate::domain::plugin::{AuthContext, AuthPlugin, PluginError};

/// Configuration for the API key auth plugin.
#[derive(Debug, Deserialize)]
struct ApiKeyConfig {
    /// Header name to set (e.g. "Authorization", "X-API-Key").
    header: String,
    /// Prefix prepended to the secret value (e.g. "Bearer ").
    #[serde(default)]
    prefix: String,
    /// Secret reference to resolve (e.g. "cred://openai-key").
    secret_ref: String,
}

/// Auth plugin that resolves a secret reference and injects it as a header value.
pub struct ApiKeyAuthPlugin {
    credstore: Arc<dyn CredStoreClientV1>,
}

impl ApiKeyAuthPlugin {
    /// Create the plugin, resolving secrets through the given credential store.
    #[must_use]
    pub fn new(credstore: Arc<dyn CredStoreClientV1>) -> Self {
        Self { credstore }
    }
}

#[async_trait]
impl AuthPlugin for ApiKeyAuthPlugin {
    async fn authenticate(&self, ctx: &mut AuthContext) -> Result<(), PluginError> {
        let config: ApiKeyConfig = serde_json::from_value(
            serde_json::to_value(&ctx.config)
                .map_err(|e| PluginError::Internal(format!("invalid apikey auth config: {e}")))?,
        )
        .map_err(|e| PluginError::Internal(format!("invalid apikey auth config: {e}")))?;

        let raw_ref = config
            .secret_ref
            .strip_prefix("cred://")
            .unwrap_or(&config.secret_ref);
        // Errors name the config key: the reference and the store's own text
        // (which may quote it) stay out of errors and logs (F0008).
        let key = SecretRef::new(raw_ref).map_err(|e| {
            PluginError::Internal(format!(
                "auth.config.{SECRET_REF_KEY}: {}",
                credstore_error_kind(&e)
            ))
        })?;

        let response = self
            .credstore
            .get(&ctx.security_context, &key)
            .await
            .map_err(|e| {
                PluginError::Internal(format!(
                    "credential store error for auth.config.{SECRET_REF_KEY}: {}",
                    credstore_error_kind(&e)
                ))
            })?
            .ok_or(PluginError::SecretNotFound(SECRET_REF_KEY))?;

        let secret_str = std::str::from_utf8(response.value.as_bytes())
            .map_err(|_| PluginError::Internal("secret value is not valid UTF-8".into()))?
            .to_string();

        let value = format!("{}{}", config.prefix, secret_str);
        ctx.set_headers.push((config.header, value));

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use toolkit_security::SecurityContext;
    use uuid::Uuid;

    use crate::domain::plugin::{AuthContext, AuthPlugin, PluginError};
    use crate::domain::test_support::MockCredStoreClient;

    use super::*;

    fn make_config(header: &str, prefix: &str, secret_ref: &str) -> HashMap<String, String> {
        HashMap::from([
            ("header".into(), header.into()),
            ("prefix".into(), prefix.into()),
            (SECRET_REF_KEY.into(), secret_ref.into()),
        ])
    }

    fn test_security_context() -> SecurityContext {
        SecurityContext::builder()
            .subject_tenant_id(Uuid::new_v4())
            .subject_id(Uuid::new_v4())
            .build()
            .expect("test security context")
    }

    fn make_auth_ctx(config: HashMap<String, String>) -> AuthContext {
        AuthContext::new(config, test_security_context())
    }

    #[tokio::test]
    async fn injects_bearer_token() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(vec![(
            "openai-key".into(),
            "sk-abc123".into(),
        )]));
        let plugin = ApiKeyAuthPlugin::new(credstore);

        let mut ctx = make_auth_ctx(make_config("authorization", "Bearer ", "cred://openai-key"));

        plugin.authenticate(&mut ctx).await.unwrap();
        assert_eq!(
            ctx.header_value("authorization").unwrap(),
            "Bearer sk-abc123"
        );
    }

    #[tokio::test]
    async fn injects_custom_header_no_prefix() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(vec![(
            "custom-key".into(),
            "my-secret-key".into(),
        )]));
        let plugin = ApiKeyAuthPlugin::new(credstore);

        let mut ctx = make_auth_ctx(make_config("x-api-key", "", "cred://custom-key"));

        plugin.authenticate(&mut ctx).await.unwrap();
        assert_eq!(ctx.header_value("x-api-key").unwrap(), "my-secret-key");
    }

    #[tokio::test]
    async fn prefix_stripping_cred_scheme() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(vec![(
            "my-key".into(),
            "secret-value".into(),
        )]));
        let plugin = ApiKeyAuthPlugin::new(credstore);

        // With cred:// prefix
        let mut ctx = make_auth_ctx(make_config("x-api-key", "", "cred://my-key"));
        plugin.authenticate(&mut ctx).await.unwrap();
        assert_eq!(ctx.header_value("x-api-key").unwrap(), "secret-value");
    }

    #[tokio::test]
    async fn secret_ref_without_prefix_works() {
        let credstore = Arc::new(MockCredStoreClient::with_secrets(vec![(
            "plain-key".into(),
            "plain-value".into(),
        )]));
        let plugin = ApiKeyAuthPlugin::new(credstore);

        // Without cred:// prefix
        let mut ctx = make_auth_ctx(make_config("x-api-key", "", "plain-key"));
        plugin.authenticate(&mut ctx).await.unwrap();
        assert_eq!(ctx.header_value("x-api-key").unwrap(), "plain-value");
    }

    /// A missing secret names the config key, not the reference.
    #[tokio::test]
    async fn secret_not_found_names_the_key() {
        let credstore = Arc::new(MockCredStoreClient::empty());
        let plugin = ApiKeyAuthPlugin::new(credstore);

        let mut ctx = make_auth_ctx(make_config("authorization", "Bearer ", "cred://missing"));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(matches!(err, PluginError::SecretNotFound(SECRET_REF_KEY)));
        let text = err.to_string();
        assert!(text.contains("auth.config.secret_ref"), "{text}");
        assert!(!text.contains("missing"), "{text}");
    }

    /// A credential store failure, or a malformed reference, names the
    /// config key; neither the reference nor the store's text (which may
    /// quote it) is in the error.
    #[tokio::test]
    async fn credstore_error_maps_to_internal_without_the_ref() {
        use crate::domain::test_support::ErroringCredStore;
        use credstore_sdk::CredStoreError;
        type MakeError = fn() -> CredStoreError;

        let errors: [MakeError; 2] = [
            || CredStoreError::Internal("store failed reading some-key".into()),
            || CredStoreError::ServiceUnavailable {
                detail: "some-key: backend down".into(),
                retry_after: None,
            },
        ];
        for error in errors {
            let plugin = ApiKeyAuthPlugin::new(Arc::new(ErroringCredStore(error)));
            let mut ctx = make_auth_ctx(make_config("authorization", "Bearer ", "cred://some-key"));
            let err = plugin.authenticate(&mut ctx).await.unwrap_err();
            let PluginError::Internal(text) = &err else {
                panic!("expected Internal, got {err:?}");
            };
            assert!(text.contains("auth.config.secret_ref"), "{text}");
            assert!(!text.contains("some-key"), "{text}");
        }

        let plugin = ApiKeyAuthPlugin::new(Arc::new(MockCredStoreClient::empty()));
        let mut ctx = make_auth_ctx(make_config("authorization", "", "cred://bad ref!"));
        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        let PluginError::Internal(text) = &err else {
            panic!("expected Internal, got {err:?}");
        };
        assert!(text.contains("auth.config.secret_ref"), "{text}");
        assert!(!text.contains("bad ref"), "{text}");
    }

    #[tokio::test]
    async fn invalid_utf8_in_secret_returns_internal_error() {
        // Any reference resolves to non-UTF-8 bytes.
        let plugin =
            ApiKeyAuthPlugin::new(Arc::new(MockCredStoreClient::returning_raw_value(vec![
                0xFF, 0xFE,
            ])));
        let mut ctx = make_auth_ctx(make_config("authorization", "", "cred://bad-utf8"));

        let err = plugin.authenticate(&mut ctx).await.unwrap_err();
        assert!(matches!(err, PluginError::Internal(_)));
    }
}
