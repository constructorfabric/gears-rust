//! Integration: `EcbRateProvider` end-to-end over a local axum server serving
//! the ECB daily-XML fixture. Covers the HTTP path the unit tests skip.
#![allow(
    clippy::unwrap_used,
    reason = "integration-test setup helpers: an unwrap here just fails the test"
)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use bss_ledger_sdk::{CurrencyPair, RateProviderError, RateProviderV1};
use bss_rate_provider_ecb_plugin::source::EcbRateProvider;
use bss_rate_provider_sdk::metrics::NoopFetchMetrics;
use toolkit_http::HttpClient;
use toolkit_security::SecurityContext;

const FIXTURE: &str = include_str!("fixtures/eurofxref-daily.xml");

/// Spawns the fake ECB server and returns its URL plus the server task's
/// `JoinHandle` — never drop a `tokio::spawn` handle silently, it's the only
/// way to observe the mock server panicking instead of the test just hanging
/// or failing with a confusing connection error.
async fn spawn_server(body: &'static str, status: u16) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/eurofxref-daily.xml",
        get(move || async move { (StatusCode::from_u16(status).unwrap(), body) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/eurofxref-daily.xml"), handle)
}

fn provider(url: String) -> EcbRateProvider {
    // Loopback is plain HTTP; a non-FIPS test client allows insecure http.
    let client = HttpClient::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    EcbRateProvider::new("ecb".to_owned(), url, client, Arc::new(NoopFetchMetrics))
}

#[tokio::test]
async fn full_fetch_returns_whole_table() {
    let (url, _server) = spawn_server(FIXTURE, 200).await;
    let p = provider(url);
    let ctx = SecurityContext::anonymous();
    let rates = p.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert_eq!(rates.len(), 3);
}

#[tokio::test]
async fn specific_pair_filter() {
    let (url, _server) = spawn_server(FIXTURE, 200).await;
    let p = provider(url);
    let ctx = SecurityContext::anonymous();
    let want = vec![CurrencyPair {
        base: "EUR".to_owned(),
        quote: "USD".to_owned(),
    }];
    let rates = p.fetch_latest(&ctx, &want, "req").await.unwrap();
    assert_eq!(rates.len(), 1);
    assert_eq!(rates[0].quote, "USD");
}

#[tokio::test]
async fn upstream_503_maps_to_upstream_status() {
    let (url, _server) = spawn_server(FIXTURE, 503).await;
    let p = provider(url);
    let ctx = SecurityContext::anonymous();
    let err = p.fetch_latest(&ctx, &[], "req").await.unwrap_err();
    assert!(matches!(err, RateProviderError::UpstreamStatus(503)));
}

#[tokio::test]
async fn health_probe_succeeds_on_200() {
    let (url, _server) = spawn_server(FIXTURE, 200).await;
    let p = provider(url);
    let ctx = SecurityContext::anonymous();
    p.health(&ctx, "req").await.unwrap();
}

#[tokio::test]
async fn health_probe_maps_503_to_upstream_status() {
    let (url, _server) = spawn_server(FIXTURE, 503).await;
    let p = provider(url);
    let ctx = SecurityContext::anonymous();
    let err = p.health(&ctx, "req").await.unwrap_err();
    assert!(matches!(err, RateProviderError::UpstreamStatus(503)));
}

#[tokio::test]
async fn connection_refused_maps_to_unreachable() {
    // Bind then immediately drop the listener: the port is free, but nothing
    // is listening, so any request to it fails at the transport level — the
    // network-failure path the other tests here don't exercise.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let p = provider(format!("http://{addr}/eurofxref-daily.xml"));
    let ctx = SecurityContext::anonymous();
    let err = p.fetch_latest(&ctx, &[], "req").await.unwrap_err();
    assert!(matches!(err, RateProviderError::Unreachable(_)));
}
