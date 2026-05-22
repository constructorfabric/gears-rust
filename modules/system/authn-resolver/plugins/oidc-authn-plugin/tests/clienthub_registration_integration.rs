#![allow(clippy::expect_used, clippy::panic)]

//! Integration tests for `ClientHub` registration and plugin wiring behavior.

use std::sync::Arc;

use authn_resolver_sdk::{
    AuthNResolverError, AuthNResolverPluginClient, AuthNResolverPluginSpecV1,
};
use modkit::client_hub::{ClientHub, ClientScope};
use oidc_authn_plugin::authenticate::OidcAuthNPlugin;
use oidc_authn_plugin::config::INSTANCE_SUFFIX;
use secrecy::ExposeSecret;
use uuid::Uuid;

pub mod common;

fn make_plugin(trusted_issuer: String) -> OidcAuthNPlugin {
    let issuer_trust =
        common::exact_issuer_trust(trusted_issuer).expect("trust config should build");
    common::build_test_plugin(
        common::base_jwt_validation_config(),
        issuer_trust,
        common::plugin_config(),
        reqwest::Client::new(),
    )
}

fn instance_scope() -> ClientScope {
    let instance_id = AuthNResolverPluginSpecV1::gts_make_instance_id(INSTANCE_SUFFIX);
    ClientScope::gts_id(instance_id.as_ref())
}

#[tokio::test]
async fn registered_plugin_is_discoverable_by_canonical_instance_scope() -> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let hub = ClientHub::new();
    let issuer = server.issuer("realms/platform");
    let plugin = Arc::new(make_plugin(issuer));

    let scope = plugin.register(&hub)?;
    let canonical_scope = instance_scope();
    assert!(
        scope.as_str() == canonical_scope.as_str(),
        "register() should return canonical instance-id scope"
    );
    assert!(
        hub.try_get_scoped::<dyn AuthNResolverPluginClient>(&canonical_scope)
            .is_some(),
        "registered plugin should be discoverable by canonical instance-id scope"
    );
    Ok(())
}

#[tokio::test]
async fn scoped_plugin_authenticates_valid_jwt() -> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let issuer = server.issuer("realms/platform");
    let hub = ClientHub::new();

    let plugin = Arc::new(common::build_test_plugin(
        common::base_jwt_validation_config(),
        common::exact_issuer_trust(issuer.clone()).expect("trust config should build"),
        common::plugin_config(),
        reqwest::Client::new(),
    ));
    plugin.register(&hub)?;

    assert!(plugin.is_registered());

    let discovered = hub
        .try_get_scoped::<dyn AuthNResolverPluginClient>(&instance_scope())
        .ok_or_else(|| anyhow::anyhow!("registered plugin should be discoverable"))?;

    let token = common::sign_jwt(
        &serde_json::json!({
            "sub": "550e8400-e29b-41d4-a716-446655440000",
            "tenant_id": "550e8400-e29b-41d4-a716-446655440111",
            "user_type": "user",
            "azp": "cyber-fabric-portal",
            "scope": "read write",
            "iss": issuer,
            "exp": common::future_exp()
        }),
        Some(common::TEST_KID),
    );
    let result = discovered.authenticate(&token).await?;

    assert_eq!(
        result.security_context.subject_id(),
        Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")?
    );
    assert_eq!(
        result.security_context.subject_tenant_id(),
        Uuid::parse_str("550e8400-e29b-41d4-a716-446655440111")?
    );
    assert_eq!(result.security_context.subject_type(), Some("user"));
    assert_eq!(
        result.security_context.token_scopes(),
        &["read".to_owned(), "write".to_owned()]
    );
    assert_eq!(
        result
            .security_context
            .bearer_token()
            .map(ExposeSecret::expose_secret),
        Some(token.as_str())
    );
    Ok(())
}

#[tokio::test]
async fn duplicate_register_on_same_instance_returns_error() -> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let hub = ClientHub::new();
    let plugin = Arc::new(make_plugin(server.issuer("realms/platform")));

    plugin.register(&hub)?;

    let duplicate = plugin.register(&hub);
    assert!(
        matches!(duplicate, Err(AuthNResolverError::Internal(_))),
        "second registration on same instance should fail: {duplicate:?}"
    );
    Ok(())
}
