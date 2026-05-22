#![allow(clippy::expect_used, clippy::panic)]
//! Integration tests for S2S client credentials exchange.
//!
//! Uses `MockOidcServer` with its token endpoint to verify the full
//! exchange flow: credentials -> token endpoint -> JWT validation -> `SecurityContext`.

use authn_resolver_sdk::{AuthNResolverError, AuthNResolverPluginClient, ClientCredentialsRequest};
use oidc_authn_plugin::authenticate::OidcAuthNPlugin;
use oidc_authn_plugin::claim_mapper;
use oidc_authn_plugin::config::S2sConfig;
use secrecy::SecretString;

pub mod common;

fn make_request(client_id: &str, client_secret: &str) -> ClientCredentialsRequest {
    ClientCredentialsRequest {
        client_id: client_id.to_owned(),
        client_secret: SecretString::from(client_secret),
        scopes: vec![],
    }
}

fn parse_url(value: &str) -> reqwest::Url {
    reqwest::Url::parse(value).expect("test URL should parse")
}

fn make_plugin_with_s2s(issuer: String, s2s_config: S2sConfig) -> OidcAuthNPlugin {
    make_plugin_with_s2s_and_options(
        issuer,
        s2s_config,
        claim_mapper::ClaimMapperOptions::default(),
    )
}

fn make_plugin_with_s2s_and_options(
    issuer: String,
    s2s_config: S2sConfig,
    claim_mapper_options: claim_mapper::ClaimMapperOptions,
) -> OidcAuthNPlugin {
    let jwt_config = common::base_jwt_validation_config();
    let issuer_trust = common::exact_issuer_trust(issuer).expect("trust config should build");
    let mut plugin_config = common::plugin_config();
    plugin_config.s2s = s2s_config;
    plugin_config.claim_mapper_options = claim_mapper_options;
    common::build_test_plugin(
        jwt_config,
        issuer_trust,
        plugin_config,
        reqwest::Client::new(),
    )
}

#[tokio::test]
async fn s2s_exchange_valid_credentials_returns_security_context() -> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let issuer = server.issuer("realms/platform");
    let s2s_config = S2sConfig {
        discovery_url: parse_url(&issuer),
        ..common::default_s2s_config()
    };
    let plugin = make_plugin_with_s2s(issuer, s2s_config);

    let request = make_request(common::TEST_S2S_CLIENT_ID, common::TEST_S2S_CLIENT_SECRET);
    let result = plugin.exchange_client_credentials(&request).await;

    assert!(
        result.is_ok(),
        "valid S2S credentials should succeed: {result:?}"
    );
    let auth_result = result.unwrap();
    assert_eq!(
        auth_result.security_context.subject_tenant_id().to_string(),
        "550e8400-e29b-41d4-a716-446655440222"
    );
    assert_eq!(
        auth_result.security_context.subject_type(),
        Some(common::TEST_S2S_DEFAULT_SUBJECT_TYPE)
    );
    Ok(())
}

#[tokio::test]
async fn s2s_exchange_applies_shared_first_party_clients() -> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let issuer = server.issuer("realms/platform");
    let s2s_config = S2sConfig {
        discovery_url: parse_url(&issuer),
        ..common::default_s2s_config()
    };
    let plugin = make_plugin_with_s2s_and_options(
        issuer,
        s2s_config,
        claim_mapper::ClaimMapperOptions {
            first_party_clients: vec![common::TEST_S2S_CLIENT_ID.to_owned()],
            ..claim_mapper::ClaimMapperOptions::default()
        },
    );

    let request = make_request(common::TEST_S2S_CLIENT_ID, common::TEST_S2S_CLIENT_SECRET);
    let result = plugin.exchange_client_credentials(&request).await;

    assert!(
        result.is_ok(),
        "S2S first-party client should authenticate: {result:?}"
    );
    let auth_result = result.unwrap();
    assert_eq!(auth_result.security_context.token_scopes(), &["*"]);
    Ok(())
}

#[tokio::test]
async fn s2s_exchange_applies_shared_required_claims() -> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let issuer = server.issuer("realms/platform");
    let s2s_config = S2sConfig {
        discovery_url: parse_url(&issuer),
        ..common::default_s2s_config()
    };
    let plugin = make_plugin_with_s2s_and_options(
        issuer,
        s2s_config,
        claim_mapper::ClaimMapperOptions {
            required_claims: vec!["groups".to_owned()],
            ..claim_mapper::ClaimMapperOptions::default()
        },
    );

    let request = make_request(common::TEST_S2S_CLIENT_ID, common::TEST_S2S_CLIENT_SECRET);
    let result = plugin.exchange_client_credentials(&request).await;

    assert!(
        matches!(
            result,
            Err(AuthNResolverError::Unauthorized(ref msg)) if msg == "missing claim"
        ),
        "S2S mapping should enforce shared required claims: {result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn s2s_exchange_invalid_credentials_returns_token_acquisition_failed() -> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let issuer = server.issuer("realms/platform");
    let s2s_config = S2sConfig {
        discovery_url: parse_url(&issuer),
        ..common::default_s2s_config()
    };
    let plugin = make_plugin_with_s2s(issuer, s2s_config);

    let request = make_request("unknown-client", "bad-secret");
    let result = plugin.exchange_client_credentials(&request).await;

    assert!(
        matches!(result, Err(AuthNResolverError::TokenAcquisitionFailed(_))),
        "invalid credentials should return TokenAcquisitionFailed: {result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn s2s_exchange_via_oidc_discovery() -> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let issuer = server.issuer("realms/platform");
    // Use issuer-based discovery rather than explicit token_endpoint.
    let s2s_config = S2sConfig {
        discovery_url: parse_url(&issuer),
        ..common::default_s2s_config()
    };
    let plugin = make_plugin_with_s2s(issuer, s2s_config);

    let request = make_request(common::TEST_S2S_CLIENT_ID, common::TEST_S2S_CLIENT_SECRET);
    let result = plugin.exchange_client_credentials(&request).await;

    assert!(
        result.is_ok(),
        "S2S via OIDC discovery should succeed: {result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn s2s_exchange_unreachable_discovery_returns_service_unavailable() {
    let s2s_config = S2sConfig {
        discovery_url: parse_url("http://127.0.0.1:1"),
        ..common::default_s2s_config()
    };
    let jwt_config = common::base_jwt_validation_config();
    let issuer_trust = common::exact_issuer_trust("http://127.0.0.1:1".to_owned())
        .expect("trust config should build");
    let mut plugin_config = common::plugin_config();
    plugin_config.claim_mapper = claim_mapper::default_config();
    plugin_config.s2s_claim_mapper = claim_mapper::default_config();
    plugin_config.circuit_breaker = Some(common::default_circuit_breaker_config());
    plugin_config.retry_policy = common::default_retry_policy_config();
    plugin_config.s2s = s2s_config;
    let plugin = common::build_test_plugin(
        jwt_config,
        issuer_trust,
        plugin_config,
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap(),
    );

    let request = make_request("svc", "secret");
    let result = plugin.exchange_client_credentials(&request).await;

    assert!(
        matches!(result, Err(AuthNResolverError::ServiceUnavailable(_))),
        "unreachable endpoint should return ServiceUnavailable: {result:?}"
    );
}
