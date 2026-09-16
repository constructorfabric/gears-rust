//! Integration tests for api-contracts local and HTTP clients.
//!
//! The HTTP transport is now the macro-generated `PaymentApiRestClient`
//! produced by `#[toolkit::rest_contract]`. The local transport stays a
//! hand-written adapter so we can also exercise the policy stack.

#![allow(clippy::unwrap_used)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::sync::Arc;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::models::{ChargeRequest, ListPaymentsFilter, PaymentStatus};
use api_contracts_sdk::rest::PaymentApiRestClient;
use axum::Router;
use futures_util::StreamExt;
use toolkit::ClientHub;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::policy::PolicyStack;
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_security::SecurityContext;

use cf_api_contracts::api::rest::routes::register_routes;
use cf_api_contracts::domain::local_client::PaymentLocalClient;
use cf_api_contracts::domain::service::PaymentDomainService;

fn test_ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

fn sample_charge_request() -> ChargeRequest {
    ChargeRequest::new(1000, "USD", "Test payment")
}

fn empty_policies() -> Arc<PolicyStack> {
    Arc::new(PolicyStack::new())
}

// --- Local client tests ---

#[tokio::test]
async fn local_charge_returns_pending() {
    let svc = Arc::new(PaymentDomainService::new());
    let client = PaymentLocalClient::new(Arc::clone(&svc), empty_policies());

    let resp = client
        .charge(test_ctx(), sample_charge_request())
        .await
        .unwrap();
    assert_eq!(resp.status, PaymentStatus::Pending);
    assert!(!resp.payment_id.is_nil());
}

#[tokio::test]
async fn local_get_invoice_not_found() {
    let svc = Arc::new(PaymentDomainService::new());
    let client = PaymentLocalClient::new(svc, empty_policies());

    let err = client
        .get_invoice(test_ctx(), uuid::Uuid::new_v4().to_string())
        .await
        .unwrap_err();
    assert!(matches!(err, CanonicalError::NotFound { .. }));
}

#[tokio::test]
async fn local_charge_then_get_invoice() {
    let svc = Arc::new(PaymentDomainService::new());
    let client = PaymentLocalClient::new(Arc::clone(&svc), empty_policies());

    let charge_resp = client
        .charge(test_ctx(), sample_charge_request())
        .await
        .unwrap();

    let invoice = client
        .get_invoice(test_ctx(), charge_resp.payment_id.to_string())
        .await
        .unwrap();
    assert_eq!(invoice.payment_id, charge_resp.payment_id);
    assert_eq!(invoice.amount_cents, 1000);
    assert_eq!(invoice.currency, "USD");
}

#[tokio::test]
async fn local_list_payments_empty() {
    let svc = Arc::new(PaymentDomainService::new());
    let client = PaymentLocalClient::new(svc, empty_policies());

    let items: Vec<_> = client
        .list_payments(test_ctx(), ListPaymentsFilter::default())
        .collect()
        .await;
    assert!(items.is_empty());
}

#[tokio::test]
async fn local_list_payments_with_filter() {
    let svc = Arc::new(PaymentDomainService::new());
    let client = PaymentLocalClient::new(Arc::clone(&svc), empty_policies());

    let usd = ChargeRequest::new(500, "USD", "usd1");
    let eur = ChargeRequest::new(300, "EUR", "eur1");
    client.charge(test_ctx(), usd.clone()).await.unwrap();
    client.charge(test_ctx(), usd).await.unwrap();
    client.charge(test_ctx(), eur).await.unwrap();

    let filter = ListPaymentsFilter::new(None, Some("EUR".to_owned()));
    let items: Vec<_> = client
        .list_payments(test_ctx(), filter)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].currency, "EUR");
}

// --- ClientHub-based wiring tests ---

#[tokio::test]
async fn client_hub_resolves_local_client() {
    let client_hub = Arc::new(ClientHub::new());

    let domain_svc = Arc::new(PaymentDomainService::new());
    let local_client: Arc<dyn PaymentApi> =
        Arc::new(PaymentLocalClient::new(domain_svc, empty_policies()));
    client_hub.register::<dyn PaymentApi>(local_client);

    let client = client_hub.get::<dyn PaymentApi>().unwrap();

    let resp = client
        .charge(test_ctx(), sample_charge_request())
        .await
        .unwrap();
    assert_eq!(resp.status, PaymentStatus::Pending);
}

// --- HTTP transport tests via macro-generated client ---

async fn start_test_server() -> (String, Arc<PaymentDomainService>) {
    let svc = Arc::new(PaymentDomainService::new());
    let local: Arc<dyn PaymentApi> =
        Arc::new(PaymentLocalClient::new(Arc::clone(&svc), empty_policies()));

    // The framework would normally inject SecurityContext via a gateway-side
    // layer; in tests we use a per-request layer that materializes an
    // anonymous SecurityContext for every request. This matches what
    // `SecurityContext::anonymous()` returns for unauthenticated calls.
    let secctx_layer = axum::middleware::from_fn(
        |mut req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| async move {
            req.extensions_mut().insert(SecurityContext::anonymous());
            next.run(req).await
        },
    );

    let openapi = OpenApiRegistryImpl::new();
    let app = register_routes(Router::new(), &openapi, local).layer(secctx_layer);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), svc)
}

#[allow(
    clippy::expect_used,
    reason = "Test-only helper: default toolkit-http HttpClient::new only fails when the underlying TLS stack cannot be constructed, which does not happen in the standard test environment. A test fixture should fail loudly rather than propagate Result."
)]
fn http_client(base_url: &str) -> PaymentApiRestClient {
    PaymentApiRestClient::new(ClientConfig::new(base_url.to_owned()))
        .expect("default toolkit-http client build is infallible in tests")
}

#[tokio::test]
async fn http_charge_returns_pending() {
    let (base_url, _svc) = start_test_server().await;
    let client = http_client(&base_url);

    let resp = PaymentApi::charge(&client, test_ctx(), sample_charge_request())
        .await
        .unwrap();
    assert_eq!(resp.status, PaymentStatus::Pending);
    assert!(!resp.payment_id.is_nil());
}

#[tokio::test]
async fn http_charge_then_get_invoice() {
    let (base_url, _svc) = start_test_server().await;
    let client = http_client(&base_url);

    let charge_resp = PaymentApi::charge(&client, test_ctx(), sample_charge_request())
        .await
        .unwrap();

    let invoice = PaymentApi::get_invoice(&client, test_ctx(), charge_resp.payment_id.to_string())
        .await
        .unwrap();
    assert_eq!(invoice.payment_id, charge_resp.payment_id);
    assert_eq!(invoice.amount_cents, 1000);
    assert_eq!(invoice.currency, "USD");
}

#[tokio::test]
async fn http_get_invoice_not_found() {
    let (base_url, _svc) = start_test_server().await;
    let client = http_client(&base_url);

    let err = PaymentApi::get_invoice(&client, test_ctx(), uuid::Uuid::new_v4().to_string())
        .await
        .unwrap_err();
    assert!(matches!(err, CanonicalError::NotFound { .. }));
}

#[tokio::test]
async fn http_list_payments_sse() {
    let (base_url, _svc) = start_test_server().await;
    let client = http_client(&base_url);

    PaymentApi::charge(&client, test_ctx(), ChargeRequest::new(500, "USD", "usd1"))
        .await
        .unwrap();
    PaymentApi::charge(&client, test_ctx(), ChargeRequest::new(600, "USD", "usd2"))
        .await
        .unwrap();
    PaymentApi::charge(&client, test_ctx(), ChargeRequest::new(300, "EUR", "eur1"))
        .await
        .unwrap();

    let items: Vec<_> =
        PaymentApi::list_payments(&client, test_ctx(), ListPaymentsFilter::default())
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
    assert_eq!(items.len(), 3);

    let eur_items: Vec<_> = PaymentApi::list_payments(
        &client,
        test_ctx(),
        ListPaymentsFilter::new(None, Some("EUR".to_owned())),
    )
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .unwrap();
    assert_eq!(eur_items.len(), 1);
    assert_eq!(eur_items[0].currency, "EUR");
}

// --- multipart/mixed feed, fallible open (#4734) ---
//
// The same items as `http_list_payments_sse` above, over the second framing and
// the second open shape, end to end: a `#[streaming(multipart_mixed)] async fn`
// contract method, its macro-generated client, the toolkit's multipart framer
// on the server, and a real HTTP hop between them. Nothing here is written by
// hand except the route registration.

#[tokio::test]
async fn http_payment_feed_multipart_round_trip() {
    let (base_url, _svc) = start_test_server().await;
    let client = http_client(&base_url);

    for (amount, currency, desc) in [
        (500, "USD", "usd1"),
        (600, "USD", "usd2"),
        (300, "EUR", "eur1"),
    ] {
        PaymentApi::charge(
            &client,
            test_ctx(),
            ChargeRequest::new(amount, currency, desc),
        )
        .await
        .unwrap();
    }

    // The open is awaited and returns `Result<Stream, _>`: by the time this
    // `?` succeeds, the connect, the status check and the multipart boundary
    // lookup have all happened.
    let stream = PaymentApi::stream_payments(&client, test_ctx(), ListPaymentsFilter::default())
        .await
        .expect("the feed must open");
    let items: Vec<_> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("no error item, and no manufactured missing-terminator error");
    assert_eq!(items.len(), 3);

    let eur = PaymentApi::stream_payments(
        &client,
        test_ctx(),
        ListPaymentsFilter::new(None, Some("EUR".to_owned())),
    )
    .await
    .expect("the feed must open")
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .unwrap();
    assert_eq!(eur.len(), 1);
    assert_eq!(eur[0].currency, "EUR");
}

/// The point of the fallible open: a filter the server rejects is an `Err` from
/// the *call*, carrying a real domain error, and no stream is produced.
///
/// The SSE sibling cannot express this — `list_payments` hands back a stream
/// synchronously, so the same failure could only arrive as that stream's first
/// item, after the caller already believes it holds a live subscription.
#[tokio::test]
async fn http_payment_feed_open_failure_is_not_a_stream_item() {
    let (base_url, _svc) = start_test_server().await;
    let client = http_client(&base_url);

    let err = PaymentApi::stream_payments(
        &client,
        test_ctx(),
        ListPaymentsFilter::new(None, Some("dollars".to_owned())),
    )
    .await
    .map(|_| ())
    .expect_err("an unacceptable filter must fail the open, not the stream");

    assert!(
        matches!(err, CanonicalError::InvalidArgument { .. }),
        "the open must surface the server's rejected-filter error as an \
         InvalidArgument, got {err:?}"
    );
}

/// **The open failure's canonical category survives the HTTP round-trip.**
///
/// The `PaymentStatus::Failed` filter stands in for event-broker's
/// `409 PositionsNotSet`: a feed over partitions with no committed position
/// can't be opened. The domain reports it as a `FailedPrecondition`
/// `CanonicalError` (each unseeded partition a precondition violation), and the
/// client reconstructs that category from the `application/problem+json` body.
///
/// A richer *typed* payload — so a consumer could branch on exactly which
/// partitions to seed — is deferred to a future canonical-error-macro change;
/// for now the category is what round-trips (#4734).
#[tokio::test]
async fn http_payment_feed_open_failure_carries_the_canonical_category() {
    let (base_url, _svc) = start_test_server().await;
    let client = http_client(&base_url);

    let err = PaymentApi::stream_payments(
        &client,
        test_ctx(),
        ListPaymentsFilter::new(Some(PaymentStatus::Failed), None),
    )
    .await
    .map(|_| ())
    .expect_err("unseeded positions must fail the open");

    assert!(
        matches!(err, CanonicalError::FailedPrecondition { .. }),
        "the open failure must arrive as a FailedPrecondition, not degrade to \
         Internal, got {err:?}"
    );
}

#[tokio::test]
async fn client_hub_resolves_http_client() {
    let (base_url, _svc) = start_test_server().await;

    let client_hub = Arc::new(ClientHub::new());
    let http_client: Arc<dyn PaymentApi> = Arc::new(
        PaymentApiRestClient::new(ClientConfig::new(base_url))
            .expect("default toolkit-http client build is infallible in tests"),
    );
    client_hub.register::<dyn PaymentApi>(http_client);

    let client = client_hub.get::<dyn PaymentApi>().unwrap();

    let resp = client
        .charge(test_ctx(), sample_charge_request())
        .await
        .unwrap();
    assert_eq!(resp.status, PaymentStatus::Pending);
}
