//! The generated gRPC client must not panic on malformed data from a peer.
//!
//! `#[derive(ProtoBridge)]` emits an infallible `From<Proto>` that `.expect()`s
//! on every `#[proto_bridge(via_string)]` field. That is fine for values this
//! process produced, but a response is peer-controlled: a single unparseable
//! UUID would abort whichever task is driving the call. The generated client
//! therefore decodes responses through `TryFromProto`, and these tests pin that
//! behaviour by serving deliberately malformed values from a hostile server.
//!
//! One test per decode site in the codegen, since they are separate code paths:
//! one-shot unary, retryable unary (decode happens inside the retry loop), and
//! server-streaming (decode happens inside an `async_stream` body).

#![cfg(feature = "grpc-client")]
#![allow(clippy::unwrap_used)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use api_contracts_sdk::contract::PaymentApi as PaymentApiContract;
use api_contracts_sdk::grpc::PaymentApiGrpcClient;
use api_contracts_sdk::grpc::stubs;
use api_contracts_sdk::grpc::stubs::payment_api_server::{PaymentApi, PaymentApiServer};
use api_contracts_sdk::models::ListPaymentsFilter;
use futures_core::Stream;
use futures_util::StreamExt;
use tokio::sync::oneshot;
use tonic::{Request, Response, Status};
use toolkit_contract::runtime::config::{ClientConfig, RetryConfig};
use toolkit_security::SecurityContext;

/// Not a UUID. Every `via_string` field below is served this value.
const MALFORMED_UUID: &str = "definitely-not-a-uuid";

/// A server that answers every RPC with a structurally valid proto message
/// whose `via_string` fields are unparseable.
struct HostilePaymentServer;

#[tonic::async_trait]
impl PaymentApi for HostilePaymentServer {
    async fn charge(
        &self,
        _request: Request<stubs::ChargeRequest>,
    ) -> Result<Response<stubs::ChargeResponse>, Status> {
        Ok(Response::new(stubs::ChargeResponse {
            payment_id: MALFORMED_UUID.to_owned(),
            status: stubs::PaymentStatus::Pending as i32,
        }))
    }

    async fn get_invoice(
        &self,
        _request: Request<stubs::GetInvoiceRequest>,
    ) -> Result<Response<stubs::Invoice>, Status> {
        Ok(Response::new(stubs::Invoice {
            amount_cents: 100,
            currency: "USD".to_owned(),
            description: "hostile".to_owned(),
            invoice_id: MALFORMED_UUID.to_owned(),
            payment_id: MALFORMED_UUID.to_owned(),
            status: stubs::PaymentStatus::Completed as i32,
        }))
    }

    type ListPaymentsStream =
        Pin<Box<dyn Stream<Item = Result<stubs::PaymentSummary, Status>> + Send + 'static>>;

    async fn list_payments(
        &self,
        _request: Request<stubs::ListPaymentsFilter>,
    ) -> Result<Response<Self::ListPaymentsStream>, Status> {
        // First item is well-formed, second is not: the stream must yield one
        // good value and then an error, rather than panicking mid-flight.
        let good = stubs::PaymentSummary {
            amount_cents: 100,
            currency: "USD".to_owned(),
            payment_id: uuid::Uuid::new_v4().to_string(),
            status: stubs::PaymentStatus::Pending as i32,
        };
        let bad = stubs::PaymentSummary {
            payment_id: MALFORMED_UUID.to_owned(),
            ..good.clone()
        };
        let stream = futures_util::stream::iter(vec![Ok(good), Ok(bad)]);
        Ok(Response::new(Box::pin(stream)))
    }
}

async fn start_hostile_server() -> (SocketAddr, oneshot::Sender<()>) {
    let svc = PaymentApiServer::new(HostilePaymentServer);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        drop(
            tonic::transport::Server::builder()
                .add_service(svc)
                .serve_with_incoming_shutdown(incoming, async {
                    drop(rx.await);
                })
                .await,
        );
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, tx)
}

async fn hostile_client() -> (PaymentApiGrpcClient, oneshot::Sender<()>) {
    let (addr, shutdown) = start_hostile_server().await;
    let cfg = ClientConfig::new(format!("http://{addr}"))
        .with_timeout(Duration::from_secs(2))
        .with_retry(RetryConfig::off());
    (PaymentApiGrpcClient::connect(cfg).await.unwrap(), shutdown)
}

#[tokio::test]
async fn one_shot_unary_returns_error_instead_of_panicking() {
    let (client, _shutdown) = hostile_client().await;

    let result = PaymentApiContract::charge(
        &client,
        SecurityContext::anonymous(),
        api_contracts_sdk::models::ChargeRequest::new(100, "USD", "rent"),
    )
    .await;

    assert!(
        result.is_err(),
        "a malformed payment_id must surface as an error, not a decoded value"
    );
}

#[tokio::test]
async fn retryable_unary_returns_error_instead_of_panicking() {
    // `get_invoice` is `#[retryable]`, so its decode runs inside the retry
    // closure — a panic there unwinds through `retry_with_backoff`.
    let (client, _shutdown) = hostile_client().await;

    let result = PaymentApiContract::get_invoice(
        &client,
        SecurityContext::anonymous(),
        uuid::Uuid::new_v4().to_string(),
    )
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn streaming_item_error_ends_stream_instead_of_panicking() {
    let (client, _shutdown) = hostile_client().await;

    let mut stream = PaymentApiContract::list_payments(
        &client,
        SecurityContext::anonymous(),
        ListPaymentsFilter::default(),
    );

    let first = stream.next().await.expect("first item");
    assert!(first.is_ok(), "the well-formed item should decode");

    let second = stream.next().await.expect("second item");
    assert!(
        second.is_err(),
        "the malformed item must terminate the stream with an error"
    );
}
