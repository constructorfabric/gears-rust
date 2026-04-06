use std::collections::HashMap;
use std::sync::Arc;

use credstore_sdk::{
    CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef, SecretValue, SharingMode,
    TenantId as CredstoreTenantId,
};
use modkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::plugin::{AuthContext, AuthPlugin, PluginError};
use crate::domain::test_support::{FailingCredStoreClient, MockCredStoreClient};

use super::*;

fn make_config(header: &str, prefix: &str, secret_ref: &str) -> HashMap<String, String> {
    HashMap::from([
        ("header".into(), header.into()),
        ("prefix".into(), prefix.into()),
        ("secret_ref".into(), secret_ref.into()),
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
    AuthContext {
        headers: HashMap::new(),
        config,
        security_context: test_security_context(),
    }
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
        ctx.headers.get("authorization").unwrap(),
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
    assert_eq!(ctx.headers.get("x-api-key").unwrap(), "my-secret-key");
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
    assert_eq!(ctx.headers.get("x-api-key").unwrap(), "secret-value");
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
    assert_eq!(ctx.headers.get("x-api-key").unwrap(), "plain-value");
}

#[tokio::test]
async fn secret_not_found_returns_error() {
    let credstore = Arc::new(MockCredStoreClient::empty());
    let plugin = ApiKeyAuthPlugin::new(credstore);

    let mut ctx = make_auth_ctx(make_config("authorization", "Bearer ", "cred://missing"));

    let err = plugin.authenticate(&mut ctx).await.unwrap_err();
    assert!(matches!(err, PluginError::SecretNotFound(_)));
}

#[tokio::test]
async fn credstore_error_maps_to_internal() {
    let plugin = ApiKeyAuthPlugin::new(Arc::new(FailingCredStoreClient));
    let mut ctx = make_auth_ctx(make_config("authorization", "Bearer ", "cred://some-key"));

    let err = plugin.authenticate(&mut ctx).await.unwrap_err();
    assert!(matches!(err, PluginError::Internal(_)));
}

#[tokio::test]
async fn invalid_utf8_in_secret_returns_internal_error() {
    struct Utf8ErrorCredStore;

    #[async_trait::async_trait]
    impl CredStoreClientV1 for Utf8ErrorCredStore {
        async fn get(
            &self,
            _ctx: &SecurityContext,
            _key: &SecretRef,
        ) -> Result<Option<GetSecretResponse>, CredStoreError> {
            Ok(Some(GetSecretResponse {
                value: SecretValue::new(vec![0xFF, 0xFE]),
                owner_tenant_id: CredstoreTenantId::nil(),
                sharing: SharingMode::default(),
                is_inherited: false,
            }))
        }
    }

    let plugin = ApiKeyAuthPlugin::new(Arc::new(Utf8ErrorCredStore));
    let mut ctx = make_auth_ctx(make_config("authorization", "", "cred://bad-utf8"));

    let err = plugin.authenticate(&mut ctx).await.unwrap_err();
    assert!(matches!(err, PluginError::Internal(_)));
}
