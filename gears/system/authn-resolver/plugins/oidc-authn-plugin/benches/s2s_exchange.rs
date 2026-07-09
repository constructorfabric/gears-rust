#![allow(clippy::expect_used)]

//! Criterion benchmark for S2S client-credentials exchange latency.
//!
//! Targets for `cpt-cf-authn-plugin-nfr-s2s-latency`:
//! - cold `S2S` token cache p95 <= 500ms, including local `IdP` token round-trip
//! - warm `S2S` token cache p95 <= 1ms
//!
//! Run with: `cargo bench -p cf-gears-oidc-authn-plugin --bench s2s_exchange`

use std::sync::Arc;
use std::time::{Duration, Instant};

use authn_resolver_sdk::{AuthNResolverPluginClient, ClientCredentialsRequest};
use criterion::{Criterion, criterion_group, criterion_main};
use httpmock::prelude::{GET, MockServer, POST};
use jsonwebtoken::Algorithm;
use oidc_authn_plugin::authenticate::OidcAuthNPlugin;
use oidc_authn_plugin::claim_mapper;
use oidc_authn_plugin::config::{
    CircuitBreakerConfig, IssuerTrustConfig, JwtValidationConfig, OidcPluginConfig,
    RetryPolicyConfig, S2sConfig, TrustedIssuerEntry, TrustedIssuerInput,
};
use oidc_authn_plugin::domain::metrics::AuthNMetrics;
use oidc_authn_plugin::infra::runtime::build_oidc_authn_plugin_allowing_insecure_http_for_tests;
use secrecy::SecretString;
use serde_json::json;

mod common;

use common::{TEST_KID, future_exp, sign_jwt, test_jwk_json};

const TEST_S2S_CLIENT_ID: &str = "svc-benchmark";
const TEST_S2S_CLIENT_SECRET: &str = "benchmark-secret-value";
const WARM_P95_TARGET: Duration = Duration::from_millis(1);
const COLD_P95_TARGET: Duration = Duration::from_millis(500);
const REALM_PATH: &str = "/realms/platform";
const DISCOVERY_PATH: &str = "/realms/platform/.well-known/openid-configuration";
const JWKS_PATH: &str = "/realms/platform/protocol/openid-connect/certs";
const TOKEN_PATH: &str = "/realms/platform/protocol/openid-connect/token";
const BENCH_S2S_DEFAULT_SUBJECT_TYPE: &str = "gts.cf.core.security.subject_user.v1~";

struct MockOidcServer {
    server: MockServer,
}

impl MockOidcServer {
    fn spawn() -> Self {
        let server = MockServer::start();
        let issuer = server.url(REALM_PATH);

        install_discovery_mock(&server, &issuer);
        install_jwks_mock(&server);
        install_token_mock(&server, &issuer);

        Self { server }
    }

    fn issuer(&self) -> String {
        self.server.url(REALM_PATH)
    }
}

fn install_discovery_mock(server: &MockServer, issuer: &str) {
    server.mock(|when, then| {
        when.method(GET).path(DISCOVERY_PATH);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "issuer": issuer,
                "jwks_uri": format!("{issuer}/protocol/openid-connect/certs"),
                "token_endpoint": format!("{issuer}/protocol/openid-connect/token"),
            }));
    });
}

fn install_jwks_mock(server: &MockServer) {
    server.mock(|when, then| {
        when.method(GET).path(JWKS_PATH);
        then.status(200)
            .header("content-type", "application/json")
            .body(test_jwk_json());
    });
}

fn install_token_mock(server: &MockServer, issuer: &str) {
    server.mock(|when, then| {
        when.method(POST)
            .path(TOKEN_PATH)
            .form_urlencoded_tuple("grant_type", "client_credentials")
            .form_urlencoded_tuple("client_id", TEST_S2S_CLIENT_ID)
            .form_urlencoded_tuple("client_secret", TEST_S2S_CLIENT_SECRET);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(token_response(issuer));
    });
}

fn token_response(issuer: &str) -> serde_json::Value {
    let claims = json!({
        "sub": "550e8400-e29b-41d4-a716-446655440333",
        "iss": issuer,
        "exp": future_exp(),
        "client_id": TEST_S2S_CLIENT_ID,
        "tenant_id": "550e8400-e29b-41d4-a716-446655440222",
        "scope": "benchmark",
    });
    json!({
        "access_token": sign_jwt(&claims, Some(TEST_KID)),
        "token_type": "Bearer",
        "expires_in": 300,
    })
}

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
        expected_audience: Vec::new(),
        jose_typ: None,
        clock_skew_leeway_secs: None,
    }])
    .map_err(anyhow::Error::msg)
}

fn retry_policy() -> RetryPolicyConfig {
    RetryPolicyConfig {
        max_attempts: 3,
        initial_backoff_ms: 100,
        max_backoff_ms: 2_000,
        jitter: true,
    }
}

fn circuit_breaker_config() -> CircuitBreakerConfig {
    CircuitBreakerConfig {
        failure_threshold: 5,
        reset_timeout_secs: 30,
    }
}

fn create_test_metrics() -> Arc<AuthNMetrics> {
    Arc::new(AuthNMetrics::new(&opentelemetry::global::meter(
        "oidc-authn-plugin.s2s-bench",
    )))
}

fn make_plugin(issuer: &str) -> OidcAuthNPlugin {
    let plugin_config = OidcPluginConfig {
        vendor: "constructorfabric".to_owned(),
        priority: 100,
        claim_mapper: claim_mapper::default_config(),
        s2s_claim_mapper: claim_mapper::default_config(),
        claim_mapper_options: claim_mapper::ClaimMapperOptions::default(),
        s2s_default_subject_type: BENCH_S2S_DEFAULT_SUBJECT_TYPE.to_owned(),
        circuit_breaker: Some(circuit_breaker_config()),
        retry_policy: retry_policy(),
        s2s: S2sConfig {
            discovery_url: reqwest::Url::parse(issuer).expect("bench issuer URL should parse"),
            token_cache_ttl_secs: 300,
            token_cache_max_entries: 10_000,
        },
    };
    build_oidc_authn_plugin_allowing_insecure_http_for_tests(
        base_jwt_validation_config(),
        exact_issuer_trust(issuer.to_owned()).expect("trust config should build"),
        plugin_config,
        reqwest::Client::new(),
        create_test_metrics(),
    )
}

fn make_request(scopes: Vec<String>) -> ClientCredentialsRequest {
    ClientCredentialsRequest {
        client_id: TEST_S2S_CLIENT_ID.to_owned(),
        client_secret: SecretString::from(TEST_S2S_CLIENT_SECRET),
        scopes,
    }
}

fn p95(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    let index = samples
        .len()
        .saturating_mul(95)
        .div_ceil(100)
        .saturating_sub(1);
    samples[index]
}

fn assert_latency_targets(rt: &tokio::runtime::Runtime, plugin: &OidcAuthNPlugin) {
    let warm_request = make_request(vec!["warm-latency".to_owned()]);
    rt.block_on(async {
        plugin
            .exchange_client_credentials(&warm_request)
            .await
            .expect("warm-up exchange should succeed");
    });

    let warm_samples = (0..200)
        .map(|_| {
            let start = Instant::now();
            rt.block_on(async {
                plugin
                    .exchange_client_credentials(&warm_request)
                    .await
                    .expect("warm cache exchange should succeed");
            });
            start.elapsed()
        })
        .collect::<Vec<_>>();
    let warm_p95 = p95(warm_samples);
    assert!(
        warm_p95 <= WARM_P95_TARGET,
        "warm S2S p95 target exceeded: p95={warm_p95:?}, target={WARM_P95_TARGET:?}"
    );

    let cold_samples = (0..50)
        .map(|index| {
            let request = make_request(vec![format!("cold-latency-{index}")]);
            let start = Instant::now();
            rt.block_on(async {
                plugin
                    .exchange_client_credentials(&request)
                    .await
                    .expect("cold cache exchange should succeed");
            });
            start.elapsed()
        })
        .collect::<Vec<_>>();
    let cold_p95 = p95(cold_samples);
    assert!(
        cold_p95 <= COLD_P95_TARGET,
        "cold S2S p95 target exceeded: p95={cold_p95:?}, target={COLD_P95_TARGET:?}"
    );
}

fn benchmark_s2s_exchange(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let server = MockOidcServer::spawn();
    let issuer = server.issuer();
    let plugin = make_plugin(&issuer);

    assert_latency_targets(&rt, &plugin);

    let warm_request = make_request(vec!["read".to_owned(), "write".to_owned()]);
    rt.block_on(async {
        plugin
            .exchange_client_credentials(&warm_request)
            .await
            .expect("warm-up exchange should populate token cache");
    });

    c.bench_function("s2s_exchange_warm_token_cache", |b| {
        b.iter(|| {
            rt.block_on(async {
                plugin
                    .exchange_client_credentials(&warm_request)
                    .await
                    .expect("warm token cache exchange should succeed")
            })
        });
    });

    let next_scope = std::cell::Cell::new(0_u64);
    c.bench_function("s2s_exchange_cold_token_cache_local_idp", |b| {
        b.iter(|| {
            let index = next_scope.get();
            next_scope.set(index + 1);
            let request = make_request(vec![format!("bench-cold-{index}")]);
            rt.block_on(async {
                plugin
                    .exchange_client_credentials(&request)
                    .await
                    .expect("cold token cache exchange should succeed")
            })
        });
    });
}

criterion_group!(benches, benchmark_s2s_exchange);
criterion_main!(benches);
