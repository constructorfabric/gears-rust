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
