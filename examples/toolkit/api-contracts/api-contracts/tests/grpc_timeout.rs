//! The generated gRPC client against a real tonic server that does not answer
//! within the configured per-call timeout: every call shape — one-shot unary,
//! retryable unary, and both stream opens — reports `Timeout` with the
//! configured value, not tonic's `CANCELLED "Timeout expired"`.

#![cfg(feature = "grpc-client")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::grpc::PaymentApiGrpcClient;
use api_contracts_sdk::grpc::stubs;
use api_contracts_sdk::grpc::stubs::payment_api_server::{
    PaymentApi as PaymentApiServerTrait, PaymentApiServer,
};
use api_contracts_sdk::models::{ChargeRequest, ListPaymentsFilter};
use futures_core::Stream;
use futures_util::StreamExt as _;
use tonic::{Request, Response, Status};
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::{ClientConfig, RetryConfig};
use toolkit_security::SecurityContext;

const CALL_TIMEOUT: Duration = Duration::from_millis(200);
const SERVER_DELAY: Duration = Duration::from_secs(10);

type ItemStream = Pin<Box<dyn Stream<Item = Result<stubs::PaymentSummary, Status>> + Send>>;

/// Answers nothing before `SERVER_DELAY`.
struct Slow;

#[tonic::async_trait]
impl PaymentApiServerTrait for Slow {
    async fn charge(
        &self,
        _request: Request<stubs::ChargeRequest>,
    ) -> Result<Response<stubs::ChargeResponse>, Status> {
        tokio::time::sleep(SERVER_DELAY).await;
        Err(Status::unavailable("too late"))
    }

    async fn get_invoice(
        &self,
        _request: Request<stubs::GetInvoiceRequest>,
    ) -> Result<Response<stubs::Invoice>, Status> {
        tokio::time::sleep(SERVER_DELAY).await;
        Err(Status::unavailable("too late"))
    }

    type ListPaymentsStream = ItemStream;

    async fn list_payments(
        &self,
        _request: Request<stubs::ListPaymentsFilter>,
    ) -> Result<Response<Self::ListPaymentsStream>, Status> {
        tokio::time::sleep(SERVER_DELAY).await;
        Err(Status::unavailable("too late"))
    }

    type StreamPaymentsStream = ItemStream;

    async fn stream_payments(
        &self,
        _request: Request<stubs::ListPaymentsFilter>,
    ) -> Result<Response<Self::StreamPaymentsStream>, Status> {
        tokio::time::sleep(SERVER_DELAY).await;
        Err(Status::unavailable("too late"))
    }
}

async fn serve() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        drop(
            tonic::transport::Server::builder()
                .add_service(PaymentApiServer::new(Slow))
                .serve_with_incoming(incoming)
                .await,
        );
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

async fn client() -> PaymentApiGrpcClient {
    let addr = serve().await;
    PaymentApiGrpcClient::connect_lazy(
        ClientConfig::new(format!("http://{addr}"))
            .with_timeout(CALL_TIMEOUT)
            .with_retry(RetryConfig::off()),
    )
    .unwrap()
}

fn assert_timed_out(what: &str, err: &CanonicalError) {
    let text = format!("{err:?}");
    assert!(
        text.contains(&format!("timeout after {CALL_TIMEOUT:?}")),
        "{what}: {text}"
    );
}

#[tokio::test]
async fn a_one_shot_unary_call_times_out() {
    let client = client().await;
    let err = PaymentApi::charge(
        &client,
        SecurityContext::anonymous(),
        ChargeRequest::new(100, "USD", "test"),
    )
    .await
    .expect_err("the server does not answer in time");
    assert_timed_out("charge", &err);
}

#[tokio::test]
async fn a_retryable_unary_call_times_out() {
    let client = client().await;
    let err = PaymentApi::get_invoice(&client, SecurityContext::anonymous(), "inv-1".to_owned())
        .await
        .expect_err("the server does not answer in time");
    assert_timed_out("get_invoice", &err);
}

#[tokio::test]
async fn an_immediate_stream_open_times_out() {
    let client = client().await;
    let mut stream = PaymentApi::list_payments(
        &client,
        SecurityContext::anonymous(),
        ListPaymentsFilter::default(),
    );
    let err = stream
        .next()
        .await
        .expect("the open failure is the first item")
        .expect_err("the server does not answer in time");
    assert_timed_out("list_payments", &err);
}

#[tokio::test]
async fn a_fallible_stream_open_times_out() {
    let client = client().await;
    let Err(err) = PaymentApi::stream_payments(
        &client,
        SecurityContext::anonymous(),
        ListPaymentsFilter::default(),
    )
    .await
    else {
        panic!("the server does not answer in time");
    };
    assert_timed_out("stream_payments", &err);
}
