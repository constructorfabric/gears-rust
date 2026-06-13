//! End-to-end gRPC integration tests for `PaymentApi`.
//!
//! Spins up an in-process tonic server with the hand-written
//! `PaymentApiGrpcService`, dials it from the macro-generated
//! `PaymentApiGrpcClient`, and exercises unary, retry, error mapping, and
//! server-streaming round-trips.

#![cfg(feature = "grpc-client")]
#![allow(clippy::unwrap_used)]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::net::SocketAddr;
use std::sync::Arc;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::grpc::PaymentApiGrpcClient;
use api_contracts_sdk::models::{ChargeRequest, ListPaymentsFilter};
use futures_util::StreamExt;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::{ClientConfig, RetryConfig};
use toolkit_security::SecurityContext;
use std::time::Duration;
use tokio::sync::oneshot;
use uuid::Uuid;

use cf_api_contracts::api::grpc::PaymentApiGrpcService;
use cf_api_contracts::domain::service::PaymentDomainService;

fn anonymous_ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

async fn start_server() -> (Arc<PaymentDomainService>, SocketAddr, oneshot::Sender<()>) {
    let domain = Arc::new(PaymentDomainService::new());
    let svc = PaymentApiGrpcService::new(Arc::clone(&domain)).into_server();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        // Background server: errors at shutdown are expected (graceful drop) and
        // not propagated to test failure — the test asserts on the client path.
        drop(
            tonic::transport::Server::builder()
                .add_service(svc)
                .serve_with_incoming_shutdown(incoming, async {
                    // rx.await Err means the sender was dropped before signalling,
                    // which is fine — the server simply runs to natural completion.
                    drop(rx.await);
                })
                .await,
        );
    });
    // Brief settle so the server is ready before we dial.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (domain, addr, tx)
}

fn client_for(addr: SocketAddr) -> ClientConfig {
    ClientConfig::new(format!("http://{addr}"))
        .with_timeout(Duration::from_secs(2))
        .with_retry(RetryConfig::off())
}

#[tokio::test]
async fn grpc_charge_returns_pending() {
    let (_domain, addr, _shutdown) = start_server().await;
    let client = PaymentApiGrpcClient::connect(client_for(addr))
        .await
        .unwrap();

    let req = ChargeRequest::new(1000, "USD", "rent");
    let resp = PaymentApi::charge(&client, anonymous_ctx(), req)
        .await
        .unwrap();
    assert_eq!(
        resp.status,
        api_contracts_sdk::models::PaymentStatus::Pending,
    );
    assert!(!resp.payment_id.is_nil());
}

#[tokio::test]
async fn grpc_charge_then_get_invoice_round_trip() {
    let (_domain, addr, _shutdown) = start_server().await;
    let client = PaymentApiGrpcClient::connect(client_for(addr))
        .await
        .unwrap();

    let charge = PaymentApi::charge(
        &client,
        anonymous_ctx(),
        ChargeRequest::new(2500, "EUR", "subscription"),
    )
    .await
    .unwrap();

    let invoice = PaymentApi::get_invoice(&client, anonymous_ctx(), charge.payment_id.to_string())
        .await
        .unwrap();
    assert_eq!(invoice.payment_id, charge.payment_id);
    assert_eq!(invoice.amount_cents, 2500);
    assert_eq!(invoice.currency, "EUR");
}

#[tokio::test]
async fn grpc_get_invoice_not_found_returns_canonical_error() {
    let (_domain, addr, _shutdown) = start_server().await;
    let client = PaymentApiGrpcClient::connect(client_for(addr))
        .await
        .unwrap();

    let bogus = Uuid::new_v4().to_string();
    let err = PaymentApi::get_invoice(&client, anonymous_ctx(), bogus)
        .await
        .unwrap_err();
    assert!(matches!(err, CanonicalError::NotFound { .. }));
}

#[tokio::test]
async fn grpc_list_payments_streams_three() {
    let (_domain, addr, _shutdown) = start_server().await;
    let client = PaymentApiGrpcClient::connect(client_for(addr))
        .await
        .unwrap();

    for amount in [100i64, 200, 300] {
        PaymentApi::charge(
            &client,
            anonymous_ctx(),
            ChargeRequest::new(amount, "USD", "test"),
        )
        .await
        .unwrap();
    }

    let stream = PaymentApi::list_payments(&client, anonymous_ctx(), ListPaymentsFilter::default());
    let items: Vec<_> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(items.len(), 3);
}

#[tokio::test]
async fn client_hub_resolves_grpc_client_as_payment_api() {
    let (_domain, addr, _shutdown) = start_server().await;
    let grpc_client = PaymentApiGrpcClient::connect(client_for(addr))
        .await
        .unwrap();

    let hub = Arc::new(ClientHub::new());
    let arc_client: Arc<dyn PaymentApi> = Arc::new(grpc_client);
    hub.register::<dyn PaymentApi>(arc_client);

    let resolved = hub.get::<dyn PaymentApi>().unwrap();
    let resp = resolved
        .charge(anonymous_ctx(), ChargeRequest::new(50, "USD", "via hub"))
        .await
        .unwrap();
    assert_eq!(
        resp.status,
        api_contracts_sdk::models::PaymentStatus::Pending,
    );
}
