//! Integration tests for api-contracts local and HTTP clients.
//!
//! The HTTP transport is now the macro-generated `PaymentServiceRestClient`
//! produced by `#[toolkit::rest_contract]`. The local transport stays a
//! hand-written adapter so we can also exercise the policy stack.

#![allow(clippy::unwrap_used)]
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
use cf_api_contracts::client::local::PaymentLocalClient;
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
