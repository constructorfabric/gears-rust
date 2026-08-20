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
use bss_rate_provider_ecb_plugin::infra::source::EcbRateProvider;
use bss_rate_provider_sdk::metrics::NoopFetchMetrics;
use toolkit_http::HttpClient;
use toolkit_security::SecurityContext;

const FIXTURE: &str = include_str!("fixtures/eurofxref-daily.xml");

/// The same document dated far enough ahead that no plausible clock skew could
/// explain it. A fixed year rather than `now() + n` keeps the fixture a
/// `&'static str`, and it stays "the future" for as long as this code exists.
const FUTURE_DATED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gesmes:Envelope xmlns:gesmes="http://www.gesmes.org/xml/2002-08-01" xmlns="http://www.ecb.int/vocabulary/2002-08-01/eurofxref">
  <Cube>
    <Cube time="9999-12-31">
      <Cube currency="USD" rate="1.0856"/>
    </Cube>
  </Cube>
</gesmes:Envelope>"#;

/// Spawns the fake ECB server and returns its URL plus the server task's
/// `JoinHandle`. Keeping the handle is the only way to observe the mock server
/// panicking, instead of the test hanging or failing on a confusing connection
/// error.
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
async fn health_probe_passes_on_a_non_success_status() {
    // The probe answers "did anything get through", and a 503 proves it did — the
    // status is logged, not returned as a failure. Reading a non-2xx as unhealthy
    // would also report a working feed as down on any endpoint that replies 405 to
    // HEAD while serving GET.
    for status in [403, 404, 405, 500, 503] {
        let (url, _server) = spawn_server(FIXTURE, status).await;
        let p = provider(url);
        let ctx = SecurityContext::anonymous();
        assert!(
            p.health(&ctx, "req").await.is_ok(),
            "{status} is the host answering, so the probe must pass"
        );
    }
}

#[tokio::test]
async fn health_probe_fails_only_when_nothing_gets_through() {
    // The other side of the bound above: a `.invalid` host (RFC 6761) can
    // never resolve, so the request dies at the transport level — the one
    // genuinely unreachable case — deterministically, with no ephemeral port
    // for another test's listener to race for.
    let p = provider("http://unreachable.invalid/eurofxref-daily.xml".to_owned());
    let ctx = SecurityContext::anonymous();
    let err = p.health(&ctx, "req").await.unwrap_err();
    assert!(
        matches!(err, RateProviderError::Unreachable(_)),
        "a transport failure must be Unreachable, got {err:?}"
    );
}

#[tokio::test]
async fn a_future_dated_document_is_rejected_rather_than_served() {
    // A future `as_of` has a negative age, so the ledger's age-based staleness
    // rule would read it as fresh forever. It must fail the fetch instead, which
    // also lets the composite fall through to the next source.
    let (url, _server) = spawn_server(FUTURE_DATED, 200).await;
    let p = provider(url);
    let ctx = SecurityContext::anonymous();
    let err = p.fetch_latest(&ctx, &[], "req").await.unwrap_err();
    assert!(
        matches!(err, RateProviderError::Internal(ref m) if m.contains("9999-12-31")),
        "expected the rejection to name the offending publication time, got {err:?}"
    );
}

#[tokio::test]
async fn unresolvable_host_maps_to_unreachable() {
    // A `.invalid` host (RFC 6761) can never resolve, so the request fails at
    // the transport level deterministically — no ephemeral port involved, so
    // no other test can ever race for it.
    let p = provider("http://unreachable.invalid/eurofxref-daily.xml".to_owned());
    let ctx = SecurityContext::anonymous();
    let err = p.fetch_latest(&ctx, &[], "req").await.unwrap_err();
    assert!(matches!(err, RateProviderError::Unreachable(_)));
}

#[tokio::test]
async fn provider_id_reports_the_configured_id_not_a_hardcoded_one() {
    // The composite reads this to attribute its logs, and the id also has to
    // survive into every rate as provenance. Returning a constant baked into the
    // source instead of the configured value would make two ECB-family feeds
    // indistinguishable in the ledger's stored rows.
    let (url, _server) = spawn_server(FIXTURE, 200).await;
    let p = provider(url);
    assert_eq!(p.provider_id(), "ecb");

    let ctx = SecurityContext::anonymous();
    let rates = p.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert!(
        rates.iter().all(|r| r.provider == p.provider_id()),
        "every served rate must carry the same id the source reports"
    );
}
