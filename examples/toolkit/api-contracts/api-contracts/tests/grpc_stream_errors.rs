//! Errors inside the server streams of the generated gRPC client: after some
//! items (with and without a canonical Problem trailer), and at the open of
//! the infallible-open `list_payments`, which the circuit breaker counts.

#![cfg(feature = "grpc-client")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use api_contracts_sdk::contract::{PaymentApi, PaymentStream};
use api_contracts_sdk::grpc::PaymentApiGrpcClient;
use api_contracts_sdk::grpc::stubs;
use api_contracts_sdk::grpc::stubs::payment_api_server::{
    PaymentApi as PaymentApiServerTrait, PaymentApiServer,
};
use api_contracts_sdk::models::{ChargeRequest, ListPaymentsFilter, PaymentSummary};
use futures_core::Stream;
use futures_util::StreamExt as _;
use tonic::{Request, Response, Status};
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_contract::runtime::config::{ClientConfig, RetryConfig};
use toolkit_resilience::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, State};
use toolkit_security::SecurityContext;

type ItemStream = Pin<Box<dyn Stream<Item = Result<stubs::PaymentSummary, Status>> + Send>>;

/// How the stub's streams fail.
#[derive(Clone, Copy)]
enum Failure {
    /// Two items, then `UNAVAILABLE` carrying a Problem with a retry hint.
    MidStreamWithProblem,
    /// Two items, then a bare `UNAVAILABLE`.
    MidStreamBare,
    /// The open itself fails with a bare `UNAVAILABLE`.
    AtOpen,
}

#[derive(Clone)]
struct Failing {
    failure: Failure,
    opens: Arc<AtomicUsize>,
}

const RETRY_AFTER: u64 = 7;

fn item(n: u8) -> stubs::PaymentSummary {
    PaymentSummary::new(
        uuid::Uuid::from_u128(u128::from(n)),
        100,
        "USD",
        api_contracts_sdk::models::PaymentStatus::Pending,
    )
    .into()
}

fn unavailable(with_problem: bool) -> Status {
    let mut status = Status::unavailable("feed lost");
    if with_problem {
        let problem = Problem::from(
            CanonicalError::service_unavailable()
                .with_retry_after_seconds(RETRY_AFTER)
                .create(),
        );
        toolkit_transport_grpc::attach_problem(status.metadata_mut(), &problem).unwrap();
    }
    status
}

impl Failing {
    fn open(&self) -> Result<Response<ItemStream>, Status> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        let with_problem = match self.failure {
            Failure::AtOpen => return Err(unavailable(false)),
            Failure::MidStreamWithProblem => true,
            Failure::MidStreamBare => false,
        };
        let items = vec![Ok(item(1)), Ok(item(2)), Err(unavailable(with_problem))];
        Ok(Response::new(Box::pin(futures_util::stream::iter(items))))
    }
}

#[tonic::async_trait]
impl PaymentApiServerTrait for Failing {
    async fn charge(
        &self,
        _request: Request<stubs::ChargeRequest>,
    ) -> Result<Response<stubs::ChargeResponse>, Status> {
        Err(Status::unimplemented("not under test"))
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
        self.open()
    }

    type StreamPaymentsStream = ItemStream;

    async fn stream_payments(
        &self,
        _request: Request<stubs::ListPaymentsFilter>,
    ) -> Result<Response<Self::StreamPaymentsStream>, Status> {
        self.open()
    }
}

async fn setup(failure: Failure) -> (Failing, PaymentApiGrpcClient, Arc<CircuitBreaker>) {
    let svc = Failing {
        failure,
        opens: Arc::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let server = PaymentApiServer::new(svc.clone());
    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        drop(
            tonic::transport::Server::builder()
                .add_service(server)
                .serve_with_incoming(incoming)
                .await,
        );
    });
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

/// Read a stream to the end: the items, then everything after them.
async fn drain(
    stream: PaymentStream<PaymentSummary>,
) -> Vec<Result<PaymentSummary, CanonicalError>> {
    stream.collect().await
}

fn retry_after(err: &CanonicalError) -> Option<u64> {
    let problem = serde_json::to_value(Problem::from(err.clone())).unwrap();
    problem["context"]["retry_after_seconds"].as_u64()
}

/// Both stream shapes: the items arrive, then one error of the provider's
/// category with the Problem's retry hint, then the end of the stream.
#[tokio::test]
async fn an_error_after_items_keeps_its_problem() {
    let (_svc, client, _breaker) = setup(Failure::MidStreamWithProblem).await;
    let ctx = SecurityContext::anonymous;

    let listed = drain(PaymentApi::list_payments(
        &client,
        ctx(),
        ListPaymentsFilter::default(),
    ))
    .await;
    let streamed = drain(
        PaymentApi::stream_payments(&client, ctx(), ListPaymentsFilter::default())
            .await
            .expect("the open succeeds"),
    )
    .await;

    for items in [listed, streamed] {
        assert_eq!(
            items.len(),
            3,
            "two items, one error, then the end: {items:?}"
        );
        assert!(items[0].is_ok() && items[1].is_ok(), "{items:?}");
        let err = items[2].as_ref().unwrap_err();
        assert!(
            matches!(err, CanonicalError::ServiceUnavailable { .. }),
            "{err:?}"
        );
        assert_eq!(
            retry_after(err),
            Some(RETRY_AFTER),
            "the Problem came through: {err:?}"
        );
    }
}

#[tokio::test]
async fn a_bare_error_after_items_is_still_unavailable() {
    let (_svc, client, _breaker) = setup(Failure::MidStreamBare).await;

    let items = drain(PaymentApi::list_payments(
        &client,
        SecurityContext::anonymous(),
        ListPaymentsFilter::default(),
    ))
    .await;
    assert_eq!(items.len(), 3, "{items:?}");
    let err = items[2].as_ref().unwrap_err();
    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "{err:?}"
    );
    assert_eq!(retry_after(err), None, "no Problem, no hint");
}

/// `list_payments` has an infallible open: a refused open is the stream's
/// first and only item. Unlike `stream_payments` it is not
/// `#[circuit_breaker(skip)]`, so the breaker counts it and then refuses a
/// unary call without sending it.
#[tokio::test]
async fn a_failed_open_is_the_first_item_and_counts_for_the_breaker() {
    let (svc, client, breaker) = setup(Failure::AtOpen).await;

    for _ in 0..2 {
        let items = drain(PaymentApi::list_payments(
            &client,
            SecurityContext::anonymous(),
            ListPaymentsFilter::default(),
        ))
        .await;
        assert_eq!(items.len(), 1, "{items:?}");
        assert!(
            matches!(items[0], Err(CanonicalError::ServiceUnavailable { .. })),
            "{items:?}"
        );
    }
    assert_eq!(breaker.state(), State::Open);
    assert_eq!(svc.opens.load(Ordering::SeqCst), 2);

    let err = PaymentApi::charge(
        &client,
        SecurityContext::anonymous(),
        ChargeRequest::new(100, "USD", "after the opens"),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{err:?}").contains("circuit breaker open"),
        "{err:?}"
    );
}
