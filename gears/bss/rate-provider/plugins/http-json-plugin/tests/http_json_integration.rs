//! Integration: `HttpJsonRateProvider` end-to-end over a local axum server —
//! covers the network path (auth headers, upstream status, transport
//! failure) the pure-function unit tests in `source_tests.rs` skip.
#![allow(
    clippy::unwrap_used,
    reason = "integration-test setup helpers: an unwrap here just fails the test"
)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use bss_ledger_sdk::{CurrencyPair, RateProviderError, RateProviderV1};
use bss_rate_provider_http_json_plugin::config::{AuthKind, Mapping};
use bss_rate_provider_http_json_plugin::source::HttpJsonRateProvider;
use bss_rate_provider_sdk::metrics::NoopFetchMetrics;
use secrecy::SecretString;
use toolkit_http::HttpClient;
use toolkit_security::SecurityContext;

/// USD-based feed, two quotes.
const BODY: &str =
    r#"{"date":"2026-07-21T00:00:00Z","rates":{"EUR":{"value":"0.92"},"GBP":{"value":"0.78"}}}"#;

fn mapping() -> Mapping {
    Mapping {
        base: "USD".to_owned(),
        rates: "rates".to_owned(),
        rate: "value".to_owned(),
        as_of: "date".to_owned(),
    }
}

/// Spawns a fake JSON rate feed. `expected_header`, if set, is required on the
/// request (name, value) — the response is 401 when it's missing/wrong, so an
/// auth-header test fails loudly instead of silently passing on an
/// unauthenticated request.
async fn spawn_server(
    status: u16,
    expected_header: Option<(&'static str, &'static str)>,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/rates",
        get(move |headers: HeaderMap| async move {
            if let Some((name, value)) = expected_header {
                let actual = headers.get(name).and_then(|v| v.to_str().ok());
                if actual != Some(value) {
                    return (StatusCode::UNAUTHORIZED, String::new());
                }
            }
            (StatusCode::from_u16(status).unwrap(), BODY.to_owned())
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/rates"), handle)
}

fn provider(url: String, api_key: Option<String>, auth: AuthKind) -> HttpJsonRateProvider {
    // Loopback is plain HTTP; a non-FIPS test client allows insecure http.
    let client = HttpClient::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    HttpJsonRateProvider::new(
        "http-json".to_owned(),
        url,
        client,
        api_key.map(SecretString::from),
        auth,
        mapping(),
        Arc::new(NoopFetchMetrics),
    )
}

#[tokio::test]
async fn full_fetch_returns_whole_document() {
    let (url, _server) = spawn_server(200, None).await;
    let p = provider(url, None, AuthKind::None);
    let ctx = SecurityContext::anonymous();
    let rates = p.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert_eq!(rates.len(), 2);
}

#[tokio::test]
async fn out_of_base_pair_is_omitted_not_synthesized() {
    let (url, _server) = spawn_server(200, None).await;
    let p = provider(url, None, AuthKind::None);
    let ctx = SecurityContext::anonymous();
    // The feed's base is USD; a EUR-base request must be omitted, not derived.
    let want = vec![CurrencyPair {
        base: "EUR".to_owned(),
        quote: "GBP".to_owned(),
    }];
    let rates = p.fetch_latest(&ctx, &want, "req").await.unwrap();
    assert!(rates.is_empty());
}

#[tokio::test]
async fn matching_pair_filter_returns_only_that_pair() {
    let (url, _server) = spawn_server(200, None).await;
    let p = provider(url, None, AuthKind::None);
    let ctx = SecurityContext::anonymous();
    let want = vec![CurrencyPair {
        base: "USD".to_owned(),
        quote: "GBP".to_owned(),
    }];
    let rates = p.fetch_latest(&ctx, &want, "req").await.unwrap();
    assert_eq!(rates.len(), 1);
    assert_eq!(rates[0].quote, "GBP");
}

#[tokio::test]
async fn bearer_auth_sends_the_authorization_header() {
    let (url, _server) = spawn_server(200, Some(("authorization", "Bearer secret-key"))).await;
    let p = provider(url, Some("secret-key".to_owned()), AuthKind::Bearer);
    let ctx = SecurityContext::anonymous();
    let rates = p.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert_eq!(rates.len(), 2);
}

#[tokio::test]
async fn header_key_auth_sends_the_custom_header() {
    let (url, _server) = spawn_server(200, Some(("x-api-key", "secret-key"))).await;
    let p = provider(url, Some("secret-key".to_owned()), AuthKind::HeaderKey);
    let ctx = SecurityContext::anonymous();
    let rates = p.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert_eq!(rates.len(), 2);
}

#[tokio::test]
async fn missing_api_key_sends_no_auth_header_and_server_rejects_it() {
    let (url, _server) = spawn_server(200, Some(("authorization", "Bearer secret-key"))).await;
    // No api_key configured -> no Authorization header is sent -> server 401s.
    let p = provider(url, None, AuthKind::Bearer);
    let ctx = SecurityContext::anonymous();
    let err = p.fetch_latest(&ctx, &[], "req").await.unwrap_err();
    assert!(matches!(err, RateProviderError::UpstreamStatus(401)));
}

#[tokio::test]
async fn upstream_503_maps_to_upstream_status() {
    let (url, _server) = spawn_server(503, None).await;
    let p = provider(url, None, AuthKind::None);
    let ctx = SecurityContext::anonymous();
    let err = p.fetch_latest(&ctx, &[], "req").await.unwrap_err();
    assert!(matches!(err, RateProviderError::UpstreamStatus(503)));
}

#[tokio::test]
async fn connection_refused_maps_to_unreachable() {
    // Bind then immediately drop the listener: the port is free, but nothing
    // is listening, so any request to it fails at the transport level.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let p = provider(format!("http://{addr}/rates"), None, AuthKind::None);
    let ctx = SecurityContext::anonymous();
    let err = p.fetch_latest(&ctx, &[], "req").await.unwrap_err();
    assert!(matches!(err, RateProviderError::Unreachable(_)));
}
