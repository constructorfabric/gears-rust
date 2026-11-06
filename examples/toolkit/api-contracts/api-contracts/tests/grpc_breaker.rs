//! The circuit breaker of the generated gRPC client against a real tonic
//! server that answers `UNAVAILABLE`: a bare status counts as the provider
//! being gone, one carrying a canonical Problem is an answer, and a
//! `#[circuit_breaker(skip)]` method is neither counted nor refused.

#![cfg(feature = "grpc-client")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::grpc::PaymentApiGrpcClient;
use api_contracts_sdk::grpc::stubs;
use api_contracts_sdk::grpc::stubs::payment_api_server::{
    PaymentApi as PaymentApiServerTrait, PaymentApiServer,
};
use api_contracts_sdk::models::{ChargeRequest, ListPaymentsFilter};
use futures_core::Stream;
use tonic::{Request, Response, Status};
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_contract::runtime::config::{ClientConfig, RetryConfig};
use toolkit_resilience::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, State};
use toolkit_security::SecurityContext;

type ItemStream = Pin<Box<dyn Stream<Item = Result<stubs::PaymentSummary, Status>> + Send>>;

/// Answers every `Charge` and `StreamPayments` with `UNAVAILABLE`, with or
/// without a canonical Problem trailer, and counts the calls that reached it.
#[derive(Clone)]
struct Unavailable {
    with_problem: bool,
    charges: Arc<AtomicUsize>,
    stream_opens: Arc<AtomicUsize>,
}

impl Unavailable {
    fn status(&self) -> Status {
        let mut status = Status::unavailable("database is down");
        if self.with_problem {
            let problem = Problem::from(
                CanonicalError::service_unavailable()
                    .with_detail("database is down")
                    .create(),
            );
            toolkit_transport_grpc::attach_problem(status.metadata_mut(), &problem).unwrap();
        }
        status
    }
}

#[tonic::async_trait]
impl PaymentApiServerTrait for Unavailable {
    async fn charge(
        &self,
        _request: Request<stubs::ChargeRequest>,
    ) -> Result<Response<stubs::ChargeResponse>, Status> {
        self.charges.fetch_add(1, Ordering::SeqCst);
        Err(self.status())
    }

    async fn get_invoice(
        &self,
        _request: Request<stubs::GetInvoiceRequest>,
    ) -> Result<Response<stubs::Invoice>, Status> {
        Err(Status::unimplemented("not under test"))
    }

    type ListPaymentsStream = ItemStream;

    async fn list_payments(
        &self,
        _request: Request<stubs::ListPaymentsFilter>,
    ) -> Result<Response<Self::ListPaymentsStream>, Status> {
        Err(Status::unimplemented("not under test"))
    }

    type StreamPaymentsStream = ItemStream;

    async fn stream_payments(
        &self,
        _request: Request<stubs::ListPaymentsFilter>,
    ) -> Result<Response<Self::StreamPaymentsStream>, Status> {
        self.stream_opens.fetch_add(1, Ordering::SeqCst);
        Err(self.status())
    }
}

async fn serve(svc: Unavailable) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        drop(
            tonic::transport::Server::builder()
                .add_service(PaymentApiServer::new(svc))
                .serve_with_incoming(incoming)
                .await,
        );
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

async fn setup(with_problem: bool) -> (Unavailable, PaymentApiGrpcClient, Arc<CircuitBreaker>) {
    let svc = Unavailable {
        with_problem,
        charges: Arc::default(),
        stream_opens: Arc::default(),
    };
    let addr = serve(svc.clone()).await;
    let breaker = Arc::new(
        CircuitBreaker::new(
            CircuitBreakerConfig::default()
                .with_minimum_calls(2)
                .with_open_for(Duration::from_secs(30)),
        )
        .unwrap(),
    );
    let client = PaymentApiGrpcClient::connect_lazy(
        ClientConfig::new(format!("http://{addr}"))
            .with_timeout(Duration::from_secs(2))
            .with_retry(RetryConfig::off())
            .with_circuit_breaker(Arc::clone(&breaker)),
    )
    .unwrap();
    (svc, client, breaker)
}

async fn charge(client: &PaymentApiGrpcClient) -> CanonicalError {
    PaymentApi::charge(
        client,
        SecurityContext::anonymous(),
        ChargeRequest::new(100, "USD", "test"),
    )
    .await
    .expect_err("the server answers UNAVAILABLE")
}

async fn open_stream(client: &PaymentApiGrpcClient) -> CanonicalError {
    match PaymentApi::stream_payments(
        client,
        SecurityContext::anonymous(),
        ListPaymentsFilter::default(),
    )
    .await
    {
        Ok(_) => panic!("the server refuses the open"),
        Err(e) => e,
    }
}

fn refused_by_breaker(err: &CanonicalError) -> bool {
    format!("{err:?}").contains("circuit breaker open")
}

#[tokio::test]
async fn a_bare_unavailable_opens_the_breaker() {
    let (svc, client, breaker) = setup(false).await;

    for _ in 0..2 {
        let err = charge(&client).await;
        assert!(!refused_by_breaker(&err), "{err:?}");
    }
    assert_eq!(breaker.state(), State::Open);

    let err = charge(&client).await;
    assert!(refused_by_breaker(&err), "{err:?}");
    assert_eq!(
        svc.charges.load(Ordering::SeqCst),
        2,
        "an open breaker refuses without calling"
    );
}

#[tokio::test]
async fn an_unavailable_problem_from_the_provider_is_not_counted() {
    let (svc, client, breaker) = setup(true).await;

    for _ in 0..5 {
        let err = charge(&client).await;
        assert!(
            matches!(err, CanonicalError::ServiceUnavailable { .. }),
            "{err:?}"
        );
        assert!(!refused_by_breaker(&err), "{err:?}");
    }
    assert_eq!(breaker.state(), State::Closed);
    assert_eq!(svc.charges.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn a_skipped_method_is_neither_counted_nor_refused() {
    let (svc, client, breaker) = setup(false).await;

    for _ in 0..3 {
        open_stream(&client).await;
    }
    assert_eq!(
        breaker.state(),
        State::Closed,
        "skipped opens are not counted"
    );

    for _ in 0..2 {
        charge(&client).await;
    }
    assert_eq!(breaker.state(), State::Open);

    let err = open_stream(&client).await;
    assert!(!refused_by_breaker(&err), "{err:?}");
    assert_eq!(
        svc.stream_opens.load(Ordering::SeqCst),
        4,
        "an open breaker still lets a skipped method through"
    );
}
