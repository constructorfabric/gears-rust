//! Client implementation for the static `AuthN` resolver plugin.
//!
//! Implements `AuthNResolverPluginClient` using the domain service.

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverError, AuthNResolverPluginClient, AuthenticationResult, ClientCredentialsRequest,
};

use super::service::Service;

#[async_trait]
impl AuthNResolverPluginClient for Service {
    async fn authenticate(
        &self,
        bearer_token: &str,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        self.authenticate(bearer_token)
            .ok_or_else(|| AuthNResolverError::Unauthorized("invalid token".to_owned()))
    }

    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        self.exchange_client_credentials(request).ok_or_else(|| {
            AuthNResolverError::TokenAcquisitionFailed("invalid client credentials".to_owned())
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use secrecy::SecretString;

    use super::*;
    use crate::config::{IdentityConfig, S2sCredentialMapping, StaticAuthNPluginConfig};

    #[tokio::test]
    async fn plugin_trait_accept_all_succeeds() {
        let service = Service::from_config(&StaticAuthNPluginConfig::default());
        let plugin: &dyn AuthNResolverPluginClient = &service;

        let result = plugin.authenticate("any-token").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn plugin_trait_empty_token_unauthorized() {
        let service = Service::from_config(&StaticAuthNPluginConfig::default());
        let plugin: &dyn AuthNResolverPluginClient = &service;

        let result = plugin.authenticate("").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AuthNResolverError::Unauthorized(_) => {}
            other => panic!("Expected Unauthorized, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn plugin_trait_s2s_valid_credentials() {
        let cfg = StaticAuthNPluginConfig {
            s2s_credentials: vec![S2sCredentialMapping {
                client_id: "svc".to_owned(),
                client_secret: SecretString::from("secret"),
                identity: IdentityConfig::default(),
            }],
            ..StaticAuthNPluginConfig::default()
        };
        let service = Service::from_config(&cfg);
        let plugin: &dyn AuthNResolverPluginClient = &service;

        let request = ClientCredentialsRequest {
            client_id: "svc".to_owned(),
            client_secret: SecretString::from("secret"),
            scopes: vec![],
        };
        let result = plugin.exchange_client_credentials(&request).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn plugin_trait_s2s_invalid_credentials() {
        let service = Service::from_config(&StaticAuthNPluginConfig::default());
        let plugin: &dyn AuthNResolverPluginClient = &service;

        let request = ClientCredentialsRequest {
            client_id: "unknown".to_owned(),
            client_secret: SecretString::from("bad"),
            scopes: vec![],
        };
        let result = plugin.exchange_client_credentials(&request).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AuthNResolverError::TokenAcquisitionFailed(_) => {}
            other => panic!("Expected TokenAcquisitionFailed, got: {other:?}"),
        }
    }
}
