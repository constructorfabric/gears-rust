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
use axum::routing::{MethodFilter, get, on};
use bss_ledger_sdk::{CurrencyPair, RateProviderError, RateProviderV1};
use bss_rate_provider_http_json_plugin::config::{Auth, Mapping};
use bss_rate_provider_http_json_plugin::infra::source::{
    HttpJsonRateProvider, HttpJsonSourceSettings,
};
use bss_rate_provider_sdk::metrics::NoopFetchMetrics;
use secrecy::SecretString;
use toolkit_http::HttpClient;
use toolkit_security::SecurityContext;

/// USD-based feed, two quotes.
const BODY: &str =
    r#"{"date":"2026-07-21T00:00:00Z","rates":{"EUR":{"value":"0.92"},"GBP":{"value":"0.78"}}}"#;

/// The same feed dated far enough ahead that no plausible clock skew could
/// explain it. A fixed year rather than `now() + n` keeps this a `&'static str`,
/// and it stays "the future" for as long as this code exists.
const FUTURE_DATED_BODY: &str =
    r#"{"date":"9999-12-31T00:00:00Z","rates":{"EUR":{"value":"0.92"}}}"#;

/// The configured `provider_id`, also the provenance stamped on every rate.
const PROVIDER: &str = "http-json";

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

/// Like [`spawn_server`], but serves an arbitrary body with no auth check —
/// for tests about the document's own content rather than the request.
async fn spawn_body_server(body: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route("/rates", get(move || async move { (StatusCode::OK, body) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/rates"), handle)
}

fn provider(url: String, auth: Auth) -> HttpJsonRateProvider {
    // Loopback is plain HTTP; a non-FIPS test client allows insecure http.
    let client = HttpClient::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    HttpJsonRateProvider::new(
        HttpJsonSourceSettings {
            id: PROVIDER.to_owned(),
            base_url: url,
            auth,
            mapping: mapping(),
        },
        client,
        Arc::new(NoopFetchMetrics),
    )
}

/// A `Bearer` auth carrying `secret-key`.
fn bearer(key: &str) -> Auth {
    Auth::Bearer {
        api_key: SecretString::from(key.to_owned()),
    }
}

/// A feed that accepts GET but **not** HEAD, so a HEAD probe gets axum's `405`.
///
/// `get()` would answer HEAD too (axum routes it to the GET handler), which is the
/// behaviour this server must not have: many real JSON APIs reject HEAD, and the
/// probe has to stay green against them.
async fn spawn_get_only_server() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/rates",
        on(
            MethodFilter::GET,
            move || async move { (StatusCode::OK, BODY) },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/rates"), handle)
}

#[tokio::test]
async fn health_probe_succeeds_when_the_feed_answers() {
    let (url, _server) = spawn_server(200, None).await;
    let p = provider(url, Auth::None {});
    let ctx = SecurityContext::anonymous();
    p.health(&ctx, "req").await.unwrap();
}

#[tokio::test]
async fn health_probe_passes_when_the_feed_rejects_head() {
    // A 405 is the host answering "not that method", which proves reachability;
    // treating it as unhealthy would report a working GET-serving feed as down.
    let (url, _server) = spawn_get_only_server().await;
    let p = provider(url, Auth::None {});
    let ctx = SecurityContext::anonymous();
    assert!(
        p.health(&ctx, "req").await.is_ok(),
        "a feed that rejects HEAD is still reachable"
    );
}

#[tokio::test]
async fn health_probe_passes_on_a_non_success_status() {
    // Same rule for a server-side failure: a 503 means the request arrived.
    let (url, _server) = spawn_server(503, None).await;
    let p = provider(url, Auth::None {});
    let ctx = SecurityContext::anonymous();
    assert!(p.health(&ctx, "req").await.is_ok());
}

#[tokio::test]
async fn health_probe_fails_only_when_nothing_gets_through() {
    // The other side of the bound: a `.invalid` host (RFC 6761) can never
    // resolve, so the request dies at the transport level — the one
    // genuinely unreachable case — deterministically, with no ephemeral port
    // for another test's listener to race for.
    let p = provider("http://unreachable.invalid/rates".to_owned(), Auth::None {});
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
    let (url, _server) = spawn_body_server(FUTURE_DATED_BODY).await;
    let p = provider(url, Auth::None {});
    let ctx = SecurityContext::anonymous();
    let err = p.fetch_latest(&ctx, &[], "req").await.unwrap_err();
    assert!(
        matches!(err, RateProviderError::Internal(ref m) if m.contains("9999-12-31")),
        "expected the rejection to name the offending publication time, got {err:?}"
    );
}

#[tokio::test]
async fn full_fetch_returns_whole_document() {
    let (url, _server) = spawn_server(200, None).await;
    let p = provider(url, Auth::None {});
    let ctx = SecurityContext::anonymous();
    let rates = p.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert_eq!(rates.len(), 2);
}

#[tokio::test]
async fn every_served_rate_carries_this_sources_provenance() {
    let (url, _server) = spawn_server(200, None).await;
    let p = provider(url, Auth::None {});
    let ctx = SecurityContext::anonymous();
    let rates = p.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert!(rates.iter().all(|r| r.provider == PROVIDER));
}

#[tokio::test]
async fn out_of_base_pair_is_omitted_not_synthesized() {
    let (url, _server) = spawn_server(200, None).await;
    let p = provider(url, Auth::None {});
    let ctx = SecurityContext::anonymous();
    // The feed's base is USD; a EUR-base request must be omitted, not derived. It
    // is the ONLY requested pair, so filtering leaves nothing, which must surface
    // as an error (never a bare `Ok([])`) so the composite falls back instead of
    // treating this as a served-but-empty tick. `PairUnavailable` — the same
    // answer the ECB source gives — keeps a routine "not served here" off the
    // fetch-error metric's "internal" label.
    let want = vec![CurrencyPair {
        base: "EUR".to_owned(),
        quote: "GBP".to_owned(),
    }];
    let err = p.fetch_latest(&ctx, &want, "req").await.unwrap_err();
    assert!(
        matches!(&err, RateProviderError::PairUnavailable { base, quote } if base == "EUR" && quote == "GBP"),
        "expected PairUnavailable naming the requested pair, got {err:?}"
    );
}

#[tokio::test]
async fn matching_pair_filter_returns_only_that_pair() {
    let (url, _server) = spawn_server(200, None).await;
    let p = provider(url, Auth::None {});
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
async fn pair_filter_is_case_insensitive_like_the_ecb_source() {
    // Both sources sit behind one `CompositeRateProvider`, so a caller passing
    // lowercase codes must not get rates from one and nothing from the other.
    let (url, _server) = spawn_server(200, None).await;
    let p = provider(url, Auth::None {});
    let ctx = SecurityContext::anonymous();
    let want = vec![CurrencyPair {
        base: "usd".to_owned(),
        quote: "gbp".to_owned(),
    }];
    let rates = p.fetch_latest(&ctx, &want, "req").await.unwrap();
    assert_eq!(rates.len(), 1);
    assert_eq!(rates[0].quote, "GBP");
}

#[tokio::test]
async fn bearer_auth_sends_the_authorization_header() {
    let (url, _server) = spawn_server(200, Some(("authorization", "Bearer secret-key"))).await;
    let p = provider(url, bearer("secret-key"));
    let ctx = SecurityContext::anonymous();
    let rates = p.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert_eq!(rates.len(), 2);
}

#[tokio::test]
async fn header_key_auth_sends_the_custom_header() {
    let (url, _server) = spawn_server(200, Some(("x-api-key", "secret-key"))).await;
    let p = provider(
        url,
        Auth::HeaderKey {
            api_key: SecretString::from("secret-key".to_owned()),
        },
    );
    let ctx = SecurityContext::anonymous();
    let rates = p.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert_eq!(rates.len(), 2);
}

#[tokio::test]
async fn no_auth_sends_no_credential_and_an_authenticated_feed_rejects_it() {
    // `Auth::None {}` against a feed that requires a key: the request goes out
    // unauthenticated and surfaces the upstream's 401 rather than being retried
    // or masked.
    let (url, _server) = spawn_server(200, Some(("authorization", "Bearer secret-key"))).await;
    let p = provider(url, Auth::None {});
    let ctx = SecurityContext::anonymous();
    let err = p.fetch_latest(&ctx, &[], "req").await.unwrap_err();
    assert!(matches!(err, RateProviderError::UpstreamStatus(401)));
}

#[tokio::test]
async fn wrong_credential_surfaces_the_upstream_status() {
    let (url, _server) = spawn_server(200, Some(("authorization", "Bearer secret-key"))).await;
    let p = provider(url, bearer("wrong-key"));
    let ctx = SecurityContext::anonymous();
    let err = p.fetch_latest(&ctx, &[], "req").await.unwrap_err();
    assert!(matches!(err, RateProviderError::UpstreamStatus(401)));
}

#[tokio::test]
async fn upstream_503_maps_to_upstream_status() {
    let (url, _server) = spawn_server(503, None).await;
    let p = provider(url, Auth::None {});
    let ctx = SecurityContext::anonymous();
    let err = p.fetch_latest(&ctx, &[], "req").await.unwrap_err();
    assert!(matches!(err, RateProviderError::UpstreamStatus(503)));
}

#[tokio::test]
async fn unresolvable_host_maps_to_unreachable() {
    // A `.invalid` host (RFC 6761) can never resolve, so the request fails at
    // the transport level deterministically — no ephemeral port involved, so
    // no other test can ever race for it.
    let p = provider("http://unreachable.invalid/rates".to_owned(), Auth::None {});
    let ctx = SecurityContext::anonymous();
    let err = p.fetch_latest(&ctx, &[], "req").await.unwrap_err();
    assert!(matches!(err, RateProviderError::Unreachable(_)));
}

#[tokio::test]
async fn provider_id_reports_the_configured_id_not_a_hardcoded_one() {
    // This id is what the operator sets per feed, and it is also the provenance
    // stamped on every rate. A hardcoded value would make two feeds onboarded
    // through this same plugin indistinguishable in the ledger's stored rows —
    // exactly the case config-only onboarding is meant to support.
    let (url, _server) = spawn_server(200, None).await;
    let p = provider(url, Auth::None {});
    assert_eq!(p.provider_id(), PROVIDER);

    let ctx = SecurityContext::anonymous();
    let rates = p.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert!(
        rates.iter().all(|r| r.provider == p.provider_id()),
        "every served rate must carry the same id the source reports"
    );
}
