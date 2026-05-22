use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use httpmock::prelude::{GET, HttpMockResponse, MockServer, POST};
use secrecy::SecretString;
use serde_json::json;

use super::*;
use crate::config::{TrustedIssuerEntry, TrustedIssuerInput, default_retry_policy_config};
use crate::domain::metrics::test_harness::MetricsHarness;
use crate::infra::circuit_breaker::{HostCircuitBreakers, STATE_OPEN, host_key};

fn make_s2s_config(discovery_url: impl AsRef<str>) -> S2sConfig {
    S2sConfig {
        discovery_url: reqwest::Url::parse(discovery_url.as_ref())
            .expect("test S2S discovery URL should parse"),
        token_cache_ttl_secs: 300,
        token_cache_max_entries: 100,
    }
}

fn make_issuer_trust(issuer: String) -> IssuerTrustConfig {
    IssuerTrustConfig::from_inputs_allowing_insecure_http_for_tests([TrustedIssuerInput {
        entry: TrustedIssuerEntry::Issuer(issuer),
        discovery_url: None,
    }])
    .expect("test issuer trust should build")
}

struct SequencedTokenEndpoint {
    server: MockServer,
    calls: Arc<AtomicUsize>,
}

impl SequencedTokenEndpoint {
    fn endpoint(&self) -> String {
        self.server.url("/token")
    }

    fn calls(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.calls)
    }
}

fn spawn_sequenced_token_endpoint(
    sequence: Vec<(u16, Option<&'static str>)>,
) -> anyhow::Result<SequencedTokenEndpoint> {
    let server = MockServer::start();
    let endpoint = server.url("/token");
    reqwest::Url::parse(&endpoint)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_mock = Arc::clone(&calls);

    server.mock(move |when, then| {
        when.method(POST).path("/token");
        then.respond_with(move |_| {
            let index = calls_for_mock.fetch_add(1, Ordering::SeqCst);
            let (status, retry_after) = sequence
                .get(index)
                .copied()
                .or_else(|| sequence.last().copied())
                .unwrap_or((500, None));
            token_endpoint_response(status, retry_after)
        });
    });

    Ok(SequencedTokenEndpoint { server, calls })
}

fn token_endpoint_response(status: u16, retry_after: Option<&str>) -> HttpMockResponse {
    let body = if status == 200 {
        json!({
            "access_token": "retry-token",
            "token_type": "Bearer",
            "expires_in": 300,
        })
    } else {
        json!({ "error": "transient" })
    };

    let mut response = HttpMockResponse::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body.to_string());
    if let Some(value) = retry_after {
        response = response.header("Retry-After", value);
    }
    response.build()
}

fn make_discovery(client: reqwest::Client) -> Arc<OidcDiscovery> {
    Arc::new(OidcDiscovery::new_allowing_insecure_http_for_tests(
        3600,
        10,
        client,
        default_retry_policy_config(),
    ))
}

fn make_request(client_id: &str, client_secret: &str) -> ClientCredentialsRequest {
    ClientCredentialsRequest {
        client_id: client_id.to_owned(),
        client_secret: SecretString::from(client_secret),
        scopes: vec![],
    }
}

fn make_request_with_scopes(
    client_id: &str,
    client_secret: &str,
    scopes: &[&str],
) -> ClientCredentialsRequest {
    ClientCredentialsRequest {
        client_id: client_id.to_owned(),
        client_secret: SecretString::from(client_secret),
        scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
    }
}

struct CountingOidcServer {
    server: MockServer,
    token_calls: Arc<AtomicUsize>,
}

impl CountingOidcServer {
    fn discovery_url(&self) -> String {
        self.server.base_url()
    }

    fn token_calls(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.token_calls)
    }
}

fn spawn_counting_oidc_server_with_issuer_suffix(
    token_delay: Duration,
    issuer_suffix: &str,
) -> anyhow::Result<CountingOidcServer> {
    let server = MockServer::start();
    let base_url = server.base_url();
    reqwest::Url::parse(&base_url)?;
    let issuer = format!("{base_url}{issuer_suffix}");
    let token_calls = Arc::new(AtomicUsize::new(0));

    let discovery_body = json!({
        "issuer": issuer,
        "jwks_uri": format!("{base_url}/certs"),
        "token_endpoint": format!("{base_url}/token"),
    });
    server.mock(move |when, then| {
        when.method(GET).path("/.well-known/openid-configuration");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(discovery_body);
    });

    let token_calls_for_mock = Arc::clone(&token_calls);
    server.mock(move |when, then| {
        when.method(POST).path("/token");
        then.respond_with(move |_| {
            token_calls_for_mock.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(token_delay);
            HttpMockResponse::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(
                    json!({
                        "access_token": "single-flight-token",
                        "token_type": "Bearer",
                        "expires_in": 300,
                    })
                    .to_string(),
                )
                .build()
        });
    });

    Ok(CountingOidcServer {
        server,
        token_calls,
    })
}

#[test]
fn normalizes_scopes_by_trimming_deduping_and_sorting() {
    let request = make_request_with_scopes("svc", "secret", &[" write ", "read", "read", ""]);
    let identity = TokenClient::cache_identity(&request);

    assert_eq!(identity.normalized_scopes, "read write");
}

#[test]
fn cache_identity_reuses_scope_permutations() {
    let first = make_request_with_scopes("svc", "secret", &["write", "read"]);
    let second = make_request_with_scopes("svc", "secret", &[" read ", "write", "read"]);

    assert_eq!(
        TokenClient::cache_identity(&first).key,
        TokenClient::cache_identity(&second).key
    );
}

#[test]
fn cache_identity_isolates_credentials_and_scope_sets() {
    let base = make_request_with_scopes("svc", "secret-a", &["read", "write"]);
    let different_secret = make_request_with_scopes("svc", "secret-b", &["read", "write"]);
    let different_scopes = make_request_with_scopes("svc", "secret-a", &["read"]);

    let base_identity = TokenClient::cache_identity(&base);
    assert_ne!(
        base_identity.key,
        TokenClient::cache_identity(&different_secret).key
    );
    assert_ne!(
        base_identity.key,
        TokenClient::cache_identity(&different_scopes).key
    );
}

#[tokio::test]
async fn post_to_unreachable_endpoint_returns_idp_unreachable() {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(200))
        .build()
        .unwrap();
    let discovery = make_discovery(client.clone());
    let config = make_s2s_config("https://s2s.example.com");
    let tc = TokenClient::new(client, discovery, config, default_retry_policy_config());

    let request = make_request("svc", "secret");
    let scopes = TokenClient::normalize_scopes(&request.scopes);
    let endpoint =
        reqwest::Url::parse("http://127.0.0.1:1/token").expect("test endpoint URL should parse");
    let result = tc
        .post_client_credentials(&endpoint, &request, &scopes)
        .await;
    assert!(
        matches!(result, Err(AuthNError::IdpUnreachable)),
        "should return IdpUnreachable: {result:?}"
    );
}

#[tokio::test]
async fn post_to_endpoint_with_unsuccessful_status_returns_typed_error() -> anyhow::Result<()> {
    let token_endpoint = spawn_sequenced_token_endpoint(vec![(401, None)])?;
    let client = reqwest::Client::new();
    let discovery = make_discovery(client.clone());
    let config = make_s2s_config("https://s2s.example.com");
    let tc = TokenClient::new(client, discovery, config, default_retry_policy_config());
    let endpoint = reqwest::Url::parse(&token_endpoint.endpoint())?;
    let request = make_request("svc", "secret");
    let scopes = TokenClient::normalize_scopes(&request.scopes);

    let result = tc
        .post_client_credentials(&endpoint, &request, &scopes)
        .await;

    assert!(matches!(
        result,
        Err(AuthNError::TokenEndpointUnsuccessfulStatus(401))
    ));
    Ok(())
}

#[tokio::test]
async fn post_to_endpoint_with_invalid_json_returns_parse_failure() -> anyhow::Result<()> {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body("not-json");
    });
    let client = reqwest::Client::new();
    let discovery = make_discovery(client.clone());
    let config = make_s2s_config("https://s2s.example.com");
    let tc = TokenClient::new(client, discovery, config, default_retry_policy_config());
    let endpoint = reqwest::Url::parse(&server.url("/token"))?;
    let request = make_request("svc", "secret");
    let scopes = TokenClient::normalize_scopes(&request.scopes);

    let result = tc
        .post_client_credentials(&endpoint, &request, &scopes)
        .await;

    assert!(matches!(result, Err(AuthNError::TokenResponseParseFailed)));
    Ok(())
}

#[tokio::test]
async fn token_endpoint_breaker_opens_after_retryable_failure_and_blocks_host() -> anyhow::Result<()>
{
    let token_endpoint = spawn_sequenced_token_endpoint(vec![(500, None)])?;
    let endpoint = token_endpoint.endpoint();
    let calls = token_endpoint.calls();
    let retry_policy = RetryPolicyConfig {
        max_attempts: 0,
        initial_backoff_ms: 1,
        max_backoff_ms: 10,
        jitter: false,
    };
    let client = reqwest::Client::new();
    let discovery = make_discovery(client.clone());
    let config = make_s2s_config("https://s2s.example.com");
    let breakers = Arc::new(HostCircuitBreakers::new(
        1,
        30,
        MetricsHarness::new().metrics(),
    ));
    let tc = TokenClient::new(client, discovery, config, retry_policy)
        .with_circuit_breakers(Arc::clone(&breakers));
    let endpoint_url = reqwest::Url::parse(&endpoint)?;
    let request = make_request("svc", "secret");
    let scopes = TokenClient::normalize_scopes(&request.scopes);

    let first = tc
        .post_client_credentials(&endpoint_url, &request, &scopes)
        .await;
    assert!(matches!(first, Err(AuthNError::IdpUnreachable)));
    assert_eq!(
        breakers.state_for_host(&host_key(&endpoint_url)),
        Some(STATE_OPEN)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let second = tc
        .post_client_credentials(&endpoint_url, &request, &scopes)
        .await;
    assert!(matches!(second, Err(AuthNError::IdpUnreachable)));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "open breaker should reject before making another token endpoint call"
    );

    Ok(())
}

#[tokio::test]
async fn cache_hit_returns_without_network() {
    let client = reqwest::Client::new();
    let discovery = make_discovery(client.clone());
    let config = make_s2s_config("https://s2s.example.com");
    let issuer_trust = make_issuer_trust("https://s2s.example.com".to_owned());
    let tc = TokenClient::new(client, discovery, config, default_retry_policy_config());

    // Inject a cached token directly.
    let request = make_request("svc-cached", "any");
    let identity = TokenClient::cache_identity(&request);
    tc.cache.insert(
        &identity.key,
        CachedToken {
            access_token: "cached-jwt".to_owned(),
            fetched_at: Instant::now(),
            effective_ttl: Duration::from_mins(5),
        },
    );

    let result = tc.exchange(&request, &issuer_trust).await;
    assert_eq!(result.unwrap(), "cached-jwt");
}

#[tokio::test]
async fn concurrent_same_key_miss_uses_single_token_endpoint_call() -> anyhow::Result<()> {
    let oidc_server = spawn_counting_oidc_server_with_issuer_suffix(Duration::from_millis(75), "")?;
    let discovery_url = oidc_server.discovery_url();
    let token_calls = oidc_server.token_calls();
    let client = reqwest::Client::new();
    let discovery = make_discovery(client.clone());
    let config = make_s2s_config(&discovery_url);
    let issuer_trust = Arc::new(make_issuer_trust(discovery_url));
    let tc = Arc::new(TokenClient::new(
        client,
        discovery,
        config,
        default_retry_policy_config(),
    ));

    let mut handles = Vec::new();
    for _ in 0..5 {
        let tc_for_task = Arc::clone(&tc);
        let issuer_trust_for_task = Arc::clone(&issuer_trust);
        handles.push(tokio::spawn(async move {
            let request = make_request_with_scopes("svc", "secret", &["write", "read", "read"]);
            tc_for_task
                .exchange(&request, issuer_trust_for_task.as_ref())
                .await
        }));
    }

    for handle in handles {
        let token = handle
            .await
            .expect("single-flight task should join")
            .expect("single-flight exchange should succeed");
        assert_eq!(token, "single-flight-token");
    }

    assert_eq!(
        token_calls.load(Ordering::SeqCst),
        1,
        "same-key concurrent misses should share one token endpoint call"
    );
    Ok(())
}

#[tokio::test]
async fn exchange_rejects_untrusted_discovery_issuer_before_posting_credentials()
-> anyhow::Result<()> {
    let oidc_server = spawn_counting_oidc_server_with_issuer_suffix(
        Duration::from_millis(0),
        "/realms/untrusted",
    )?;
    let discovery_url = oidc_server.discovery_url();
    let token_calls = oidc_server.token_calls();
    let client = reqwest::Client::new();
    let discovery = make_discovery(client.clone());
    let config = make_s2s_config(discovery_url.clone());
    let issuer_trust = make_issuer_trust(format!("{discovery_url}/realms/trusted"));
    let tc = TokenClient::new(client, discovery, config, default_retry_policy_config());

    let result = tc
        .exchange(&make_request("svc", "secret"), &issuer_trust)
        .await;

    assert!(
        matches!(result, Err(AuthNError::UntrustedIssuer)),
        "untrusted discovery issuer should fail before token endpoint POST: {result:?}"
    );
    assert_eq!(
        token_calls.load(Ordering::SeqCst),
        0,
        "client credentials must not be posted when discovery issuer is untrusted"
    );
    Ok(())
}

#[test]
fn single_flight_cleanup_preserves_reacquired_gate() {
    let client = reqwest::Client::new();
    let discovery = make_discovery(client.clone());
    let config = make_s2s_config("https://s2s.example.com");
    let tc = TokenClient::new(client, discovery, config, default_retry_policy_config());
    let key = "svc-key";

    let first = tc.single_flight_gate(key);
    let second = tc.single_flight_gate(key);

    tc.release_single_flight_gate(key, &first);
    let stored = tc
        .in_flight
        .get(key)
        .expect("reacquired gate should remain registered");
    assert!(Arc::ptr_eq(stored.value(), &second));

    drop(stored);
    tc.release_single_flight_gate(key, &second);
    assert!(!tc.in_flight.contains_key(key));
}

#[tokio::test]
async fn retries_transient_5xx_then_succeeds() -> anyhow::Result<()> {
    let token_endpoint = spawn_sequenced_token_endpoint(vec![(500, None), (200, None)])?;
    let endpoint = token_endpoint.endpoint();
    let calls = token_endpoint.calls();
    let retry_policy = RetryPolicyConfig {
        max_attempts: 2,
        initial_backoff_ms: 1,
        max_backoff_ms: 10,
        jitter: false,
    };
    let client = reqwest::Client::new();
    let discovery = make_discovery(client.clone());
    let config = make_s2s_config("https://s2s.example.com");
    let tc = TokenClient::new(client, discovery, config, retry_policy);
    let endpoint_url = reqwest::Url::parse(&endpoint)?;

    let request = make_request("svc", "secret");
    let scopes = TokenClient::normalize_scopes(&request.scopes);
    let token = tc
        .post_client_credentials(&endpoint_url, &request, &scopes)
        .await?;
    assert_eq!(token.access_token, "retry-token");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    Ok(())
}

#[tokio::test]
async fn retries_429_using_retry_after_capped_by_max_backoff() -> anyhow::Result<()> {
    let token_endpoint = spawn_sequenced_token_endpoint(vec![(429, Some("1")), (200, None)])?;
    let endpoint = token_endpoint.endpoint();
    let calls = token_endpoint.calls();
    let retry_policy = RetryPolicyConfig {
        max_attempts: 1,
        initial_backoff_ms: 1,
        max_backoff_ms: 120,
        jitter: false,
    };
    let client = reqwest::Client::new();
    let discovery = make_discovery(client.clone());
    let config = make_s2s_config("https://s2s.example.com");
    let tc = TokenClient::new(client, discovery, config, retry_policy);
    let endpoint_url = reqwest::Url::parse(&endpoint)?;

    let request = make_request("svc", "secret");
    let scopes = TokenClient::normalize_scopes(&request.scopes);
    let start = Instant::now();
    let token = tc
        .post_client_credentials(&endpoint_url, &request, &scopes)
        .await?;
    let elapsed = start.elapsed();
    assert_eq!(token.access_token, "retry-token");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        elapsed >= Duration::from_millis(90),
        "expected Retry-After cap delay (~120ms), got {elapsed:?}"
    );

    Ok(())
}
