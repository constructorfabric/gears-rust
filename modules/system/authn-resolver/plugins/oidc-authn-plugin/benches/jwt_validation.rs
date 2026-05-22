#![allow(clippy::expect_used)]

//! Criterion benchmark for JWT local validation with warm JWKS cache.
//!
//! Target: p95 ≤5ms for `validate()` with a pre-cached JWKS entry.
//! Run with: `cargo bench -p oidc-authn-plugin`

use std::sync::Arc;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use httpmock::prelude::{GET, HttpMockRequest, HttpMockResponse, MockServer};
use jsonwebtoken::Algorithm;
use oidc_authn_plugin::config::{
    IssuerTrustConfig, JwtValidationConfig, RetryPolicyConfig, TrustedIssuerEntry,
    TrustedIssuerInput,
};
use oidc_authn_plugin::domain::metrics::AuthNMetrics;
use oidc_authn_plugin::domain::validator::JwtValidator;
use oidc_authn_plugin::infra::jwks::{JwksFetcher, JwksFetcherConfig, JwksFetcherDeps};
use oidc_authn_plugin::infra::oidc::OidcDiscovery;

mod common;

use common::{TEST_KID, future_exp, sign_jwt, test_jwk_json};

fn base_jwt_validation_config() -> JwtValidationConfig {
    JwtValidationConfig {
        supported_algorithms: vec![Algorithm::RS256, Algorithm::ES256],
        clock_skew_leeway_secs: 60,
        require_audience: false,
        expected_audience: Vec::new(),
        jwks_cache_ttl_secs: 3600,
        jwks_stale_ttl_secs: 86_400,
        jwks_max_entries: 64,
        jwks_refresh_on_unknown_kid: true,
        jwks_refresh_min_interval_secs: 30,
        discovery_cache_ttl_secs: 3600,
        discovery_max_entries: 64,
    }
}

fn exact_issuer_trust(issuer: String) -> anyhow::Result<IssuerTrustConfig> {
    IssuerTrustConfig::from_inputs_allowing_insecure_http_for_tests([TrustedIssuerInput {
        entry: TrustedIssuerEntry::Issuer(issuer),
        discovery_url: None,
    }])
    .map_err(anyhow::Error::msg)
}

struct MockOidcServer {
    server: MockServer,
}

impl MockOidcServer {
    fn spawn() -> Self {
        let server = MockServer::start();

        install_discovery_mock(&server);
        install_jwks_mock(&server);

        Self { server }
    }

    fn issuer(&self, realm: &str) -> String {
        let realm = realm.trim_start_matches('/');
        format!("{}/{realm}", self.server.base_url())
    }
}

fn install_discovery_mock(server: &MockServer) {
    let base_url = server.base_url();
    server.mock(move |when, then| {
        when.method(GET)
            .path_suffix("/.well-known/openid-configuration");
        then.respond_with(move |req: &HttpMockRequest| {
            let uri = req.uri();
            let issuer_path = uri
                .path()
                .strip_suffix("/.well-known/openid-configuration")
                .unwrap_or("");
            let issuer = format!("{base_url}{issuer_path}");
            HttpMockResponse::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(
                    serde_json::json!({
                        "issuer": issuer,
                        "jwks_uri": format!("{issuer}/protocol/openid-connect/certs"),
                    })
                    .to_string(),
                )
                .build()
        });
    });
}

fn install_jwks_mock(server: &MockServer) {
    server.mock(|when, then| {
        when.method(GET)
            .path_suffix("/protocol/openid-connect/certs");
        then.status(200)
            .header("content-type", "application/json")
            .body(test_jwk_json());
    });
}

fn make_validator() -> JwtValidator {
    let retry_policy = RetryPolicyConfig {
        max_attempts: 3,
        initial_backoff_ms: 100,
        max_backoff_ms: 2_000,
        jitter: true,
    };
    let discovery = Arc::new(OidcDiscovery::new_allowing_insecure_http_for_tests(
        3600,
        10,
        reqwest::Client::new(),
        retry_policy.clone(),
    ));
    let metrics = Arc::new(AuthNMetrics::new(&opentelemetry::global::meter(
        "oidc-authn-plugin.bench",
    )));
    let fetcher = Arc::new(JwksFetcher::new(
        JwksFetcherConfig {
            ttl: Duration::from_hours(1),
            stale_ttl: Duration::from_hours(24),
            max_entries: 10,
            refresh_on_unknown_kid: true,
            refresh_min_interval: Duration::from_secs(30),
        },
        JwksFetcherDeps {
            discovery,
            client: reqwest::Client::new(),
            metrics: Arc::clone(&metrics),
            retry_policy,
        },
    ));
    JwtValidator::new(fetcher, metrics)
}

fn sign_test_jwt(issuer: &str) -> String {
    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": issuer,
        "exp": future_exp(),
        "tenant_id": "tenant-benchmark",
    });
    sign_jwt(&claims, Some(TEST_KID))
}

fn benchmark_jwt_validation(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let server = MockOidcServer::spawn();
    let issuer = server.issuer("realms/platform");
    let validator = make_validator();
    let config = base_jwt_validation_config();
    let trust = exact_issuer_trust(issuer.clone()).expect("trust config should build");
    let token = sign_test_jwt(&issuer);

    rt.block_on(async {
        validator
            .validate(&token, &config, &trust)
            .await
            .expect("warm-up validation should populate JWKS cache");
    });

    c.bench_function("jwt_validate_warm_cache", |b| {
        b.iter(|| {
            rt.block_on(async {
                validator
                    .validate(&token, &config, &trust)
                    .await
                    .expect("should validate")
            })
        });
    });
}

criterion_group!(benches, benchmark_jwt_validation);
criterion_main!(benches);
