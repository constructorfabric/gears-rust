//! The OIDC plugin as producer and the SDK `CachingAuthenticator` as consumer:
//! reuse happens only when the plugin is configured to allow it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthNResolverPluginClient, AuthenticationResult,
    CachingAuthenticator, ClientCredentialsRequest,
};
use oidc_authn_plugin::authenticate::OidcAuthNPlugin;

pub mod common;

/// The plugin behind the resolver API, counting bearer validations.
struct CountingResolver {
    plugin: OidcAuthNPlugin,
    validations: AtomicUsize,
}

#[async_trait::async_trait]
impl AuthNResolverClient for CountingResolver {
    async fn authenticate(
        &self,
        request: &authn_resolver_sdk::AuthenticationRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        self.validations.fetch_add(1, Ordering::SeqCst);
        AuthNResolverPluginClient::authenticate(&self.plugin, request).await
    }

    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        self.plugin.exchange_client_credentials(request).await
    }
}

#[expect(
    clippy::expect_used,
    reason = "test setup: a bad fixture should fail the test"
)]
fn resolver(server: &common::MockOidcServer, ttl: Duration) -> Arc<CountingResolver> {
    let issuer = server.issuer("realms/platform");
    let mut plugin_config = common::plugin_config();
    plugin_config.authenticate_result_cache_ttl = ttl;
    let plugin = common::build_test_plugin(
        common::base_jwt_validation_config(),
        common::exact_issuer_trust(issuer).expect("trust config should build"),
        plugin_config,
        reqwest::Client::new(),
    );
    Arc::new(CountingResolver {
        plugin,
        validations: AtomicUsize::new(0),
    })
}

fn token(server: &common::MockOidcServer, subject: &str) -> String {
    common::sign_jwt(
        &serde_json::json!({
            "sub": subject,
            "iss": server.issuer("realms/platform"),
            "exp": common::future_exp(),
            "tenant_id": "550e8400-e29b-41d4-a716-446655440111",
        }),
        Some(common::TEST_KID),
    )
}

#[tokio::test]
async fn the_default_producer_policy_keeps_every_call_validating() -> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let resolver = resolver(&server, Duration::ZERO);
    let authenticator = CachingAuthenticator::builder(resolver.clone()).build()?;
    let token = token(&server, "550e8400-e29b-41d4-a716-446655440000");

    for _ in 0..3 {
        let result = authenticator.authenticate(&token).await?;
        assert!(result.cache_control.is_no_store());
    }
    assert_eq!(resolver.validations.load(Ordering::SeqCst), 3);
    authenticator.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn an_explicit_producer_ttl_lets_the_authenticator_reuse_per_token() -> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let resolver = resolver(&server, Duration::from_secs(60));
    let authenticator = CachingAuthenticator::builder(resolver.clone()).build()?;
    let first = token(&server, "550e8400-e29b-41d4-a716-446655440000");
    let second = token(&server, "550e8400-e29b-41d4-a716-446655440001");

    let a = authenticator.authenticate(&first).await?;
    let again = authenticator.authenticate(&first).await?;
    let b = authenticator.authenticate(&second).await?;
    assert_eq!(
        a.security_context.subject_id(),
        again.security_context.subject_id()
    );
    assert_ne!(
        a.security_context.subject_id(),
        b.security_context.subject_id()
    );
    assert_eq!(
        resolver.validations.load(Ordering::SeqCst),
        2,
        "one validation per token"
    );

    // A downstream rejection of `a` forces its token to be validated again.
    assert!(authenticator.invalidate(&a));
    authenticator.authenticate(&first).await?;
    assert_eq!(resolver.validations.load(Ordering::SeqCst), 3);

    // Rejected tokens are never cached.
    for _ in 0..2 {
        assert!(authenticator.authenticate("not-a-jwt").await.is_err());
    }
    assert_eq!(resolver.validations.load(Ordering::SeqCst), 5);
    authenticator.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn the_plugin_validates_freshly_whatever_cache_control_it_is_given() -> anyhow::Result<()> {
    use authn_resolver_sdk::{AuthenticationRequest, CacheTag, RequestCacheControl};

    let server = common::MockOidcServer::spawn()?;
    let resolver = resolver(&server, Duration::from_secs(60));
    let valid = token(&server, "550e8400-e29b-41d4-a716-446655440000");
    let modes = [
        RequestCacheControl::AllowCached,
        RequestCacheControl::refresh(),
        RequestCacheControl::refresh_unless_newer_than(CacheTag::new("unknown")),
    ];

    for mode in modes {
        let request = AuthenticationRequest::new(valid.as_str()).with_cache_control(mode.clone());
        let result = resolver.authenticate(&request).await?;
        assert!(result.cache_control.cache_until.is_some(), "{mode:?}");
        assert_eq!(result.cache_control.tag, None, "no plugin cache, no tag");
        // Invalid tokens fail in every mode.
        let rejected = AuthenticationRequest::new("not-a-jwt").with_cache_control(mode.clone());
        assert!(resolver.authenticate(&rejected).await.is_err(), "{mode:?}");
    }
    Ok(())
}
