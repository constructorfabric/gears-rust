use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::{CredStoreClientV1, SecretRef};
use serde::Deserialize;

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
        let key = SecretRef::new(raw_ref)
            .map_err(|e| PluginError::Internal(format!("invalid secret ref '{raw_ref}': {e}")))?;

        let response = self
            .credstore
            .get(&ctx.security_context, &key)
            .await
            .map_err(|e| PluginError::Internal(format!("credstore error: {e}")))?
            .ok_or_else(|| PluginError::SecretNotFound(config.secret_ref.clone()))?;

        let secret_str = std::str::from_utf8(response.value.as_bytes())
            .map_err(|_| PluginError::Internal("secret value is not valid UTF-8".into()))?
            .to_string();

        let value = format!("{}{}", config.prefix, secret_str);
        ctx.headers.insert(config.header.to_lowercase(), value);

        Ok(())
    }
}

#[cfg(test)]
#[path = "apikey_auth_tests.rs"]
mod apikey_auth_tests;
