#![allow(clippy::expect_used, clippy::panic)]
//! Integration tests for OIDC discovery and JWKS infrastructure.

use std::sync::Arc;
use std::time::Duration;

use oidc_authn_plugin::error::AuthNError;
use oidc_authn_plugin::jwks::{JwksFetcher, JwksFetcherConfig, JwksFetcherDeps};
use oidc_authn_plugin::oidc::OidcDiscovery;

pub mod common;

const TEST_ISSUER: &str = "https://oidc.example.com/realms/platform";

fn make_discovery(max_entries: usize, ttl_secs: u64) -> OidcDiscovery {
    OidcDiscovery::new_allowing_insecure_http_for_tests(
        ttl_secs,
        max_entries,
        reqwest::Client::new(),
        common::default_retry_policy_config(),
    )
}

fn assert_discovery_cache_size(discovery: &OidcDiscovery, expected: usize) {
    let debug = format!("{discovery:?}");
    assert!(
        debug.contains(&format!("cached_issuers: {expected}")),
        "expected discovery cache size {expected}, got debug output: {debug}"
    );
}

#[tokio::test]
async fn oidc_discovery_cache_evicts_oldest_entry_at_capacity() -> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let discovery = make_discovery(2, 3600);
    let issuer_1 = server.issuer("realms/issuer1");
    let issuer_2 = server.issuer("realms/issuer2");
    let issuer_3 = server.issuer("realms/issuer3");
    let issuer_1_url = reqwest::Url::parse(&issuer_1)?;
    let issuer_2_url = reqwest::Url::parse(&issuer_2)?;
    let issuer_3_url = reqwest::Url::parse(&issuer_3)?;

    discovery.get_config(&issuer_1_url).await?;
    discovery.get_config(&issuer_2_url).await?;
    discovery.get_config(&issuer_3_url).await?;

    assert_eq!(server.discovery_request_count("realms/issuer1"), 1);
    assert_eq!(server.discovery_request_count("realms/issuer2"), 1);
    assert_eq!(server.discovery_request_count("realms/issuer3"), 1);

    discovery.get_config(&issuer_2_url).await?;
    assert_eq!(
        server.discovery_request_count("realms/issuer2"),
        1,
        "newer pre-existing entry should remain cached"
    );

    discovery.get_config(&issuer_1_url).await?;
    assert_eq!(
        server.discovery_request_count("realms/issuer1"),
        2,
        "oldest entry should have been evicted and refetched"
    );
    Ok(())
}

#[tokio::test]
async fn oidc_discovery_refreshing_existing_issuer_at_capacity_keeps_cache_full()
-> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let issuer_1 = server.issuer("realms/issuer1");
    let issuer_2 = server.issuer("realms/issuer2");
    let issuer_1_url = reqwest::Url::parse(&issuer_1)?;
    let issuer_2_url = reqwest::Url::parse(&issuer_2)?;
    let discovery = make_discovery(2, 0);

    discovery.get_config(&issuer_2_url).await?;
    discovery.get_config(&issuer_1_url).await?;
    assert_discovery_cache_size(&discovery, 2);

    discovery.get_config(&issuer_1_url).await?;

    assert_eq!(server.discovery_request_count("realms/issuer1"), 2);
    assert_eq!(server.discovery_request_count("realms/issuer2"), 1);
    assert_discovery_cache_size(&discovery, 2);
    Ok(())
}

#[tokio::test]
async fn jwks_discovery_issuer_mismatch_rejects_before_jwks_fetch() -> anyhow::Result<()> {
    let server = common::MockOidcServer::spawn()?;
    let discovery_base = server.issuer("realms/attacker");
    let discovery_base_url = reqwest::Url::parse(&discovery_base)?;
    let fetcher = JwksFetcher::new(
        JwksFetcherConfig {
            ttl: Duration::from_hours(1),
            stale_ttl: Duration::from_hours(24),
            max_entries: 10,
            refresh_on_unknown_kid: true,
            refresh_min_interval: Duration::from_secs(30),
        },
        JwksFetcherDeps {
            discovery: Arc::new(make_discovery(10, 3600)),
            client: reqwest::Client::new(),
            metrics: common::create_test_metrics(),
            retry_policy: common::default_retry_policy_config(),
        },
    );

    let result = fetcher.get_jwks(TEST_ISSUER, &discovery_base_url).await;

    assert!(
        matches!(result, Err(AuthNError::UntrustedIssuer)),
        "mismatched discovery issuer must not supply JWKS for trusted issuer: {result:?}"
    );
    assert!(
        !fetcher.has_cached_entry(TEST_ISSUER),
        "mismatched discovery metadata must not populate the trusted issuer JWKS cache"
    );
    assert_eq!(server.discovery_request_count("realms/attacker"), 1);
    assert_eq!(
        server.jwks_request_count("realms/attacker"),
        0,
        "JWKS endpoint should not be fetched after issuer mismatch"
    );
    Ok(())
}
