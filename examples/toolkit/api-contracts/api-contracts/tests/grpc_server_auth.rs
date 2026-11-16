//! Bearer authentication of `PaymentApiGrpcService` with an authenticator:
//! every way a call can be refused, the status and Problem it gets, and that
//! the identity the authenticator returns is the one the domain sees.

#![cfg(feature = "grpc-client")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::grpc::PaymentApiGrpcClient;
use api_contracts_sdk::grpc::stubs;
use api_contracts_sdk::grpc::stubs::payment_api_client::PaymentApiClient;
use api_contracts_sdk::models::{ChargeRequest, ListPaymentsFilter};
use cf_api_contracts::api::grpc::PaymentApiGrpcService;
use cf_api_contracts::domain::service::PaymentDomainService;
use futures_util::StreamExt as _;
use tonic::Code;
use tonic::transport::Channel;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::{ClientConfig, RetryConfig};
use toolkit_resilience::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, State};
use toolkit_security::{AuthNError, BearerAuthenticator, DynBearerAuthenticator, SecurityContext};
use uuid::Uuid;

const TENANT_A: Uuid = Uuid::from_u128(0xa);
const TENANT_B: Uuid = Uuid::from_u128(0xb);

/// Token `tenant-a` / `tenant-b` is a user of that tenant; `down` and `boom`
/// make the backend fail; anything else is an invalid token.
struct StubAuthenticator;

impl BearerAuthenticator for StubAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        let tenant = match token {
            "tenant-a" => TENANT_A,
            "tenant-b" => TENANT_B,
            "down" => return Err(AuthNError::Unavailable),
            "boom" => return Err(AuthNError::Other("plugin crashed".to_owned())),
            _ => return Err(AuthNError::InvalidToken),
        };
        Ok(SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant)
            .bearer_token(token.to_owned())
            .build()
            .unwrap())
    }
}

async fn serve() -> SocketAddr {
    let svc = PaymentApiGrpcService::new(Arc::new(PaymentDomainService::new()))
        .with_authenticator(DynBearerAuthenticator::new(StubAuthenticator))
        .into_server();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        drop(
            tonic::transport::Server::builder()
                .add_service(svc)
                .serve_with_incoming(incoming)
                .await,
        );
    });
    addr
}

/// The tonic stub client, to send `authorization` values the generated client
/// never would.
async fn raw_client(addr: SocketAddr) -> PaymentApiClient<Channel> {
    PaymentApiClient::connect(format!("http://{addr}"))
        .await
        .unwrap()
}

async fn raw_charge(addr: SocketAddr, authorization: Option<&str>) -> tonic::Status {
    let mut request = tonic::Request::new(stubs::ChargeRequest::from(ChargeRequest::new(
        100,
        "USD",
        "auth test",
    )));
    if let Some(value) = authorization {
        request
            .metadata_mut()
            .insert("authorization", value.parse().unwrap());
    }
    raw_client(addr)
        .await
        .charge(request)
        .await
        .expect_err("the call is refused")
}

fn problem(status: &tonic::Status) -> serde_json::Value {
    toolkit_transport_grpc::extract_problem::<serde_json::Value>(status.metadata())
        .unwrap()
        .expect("every refusal carries a Problem")
}

fn reason(status: &tonic::Status) -> String {
    problem(status)["context"]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn client(addr: SocketAddr, breaker: Option<Arc<CircuitBreaker>>) -> PaymentApiGrpcClient {
    let mut config = ClientConfig::new(format!("http://{addr}"))
        .with_timeout(Duration::from_secs(2))
        .with_retry(RetryConfig::off());
    if let Some(breaker) = breaker {
        config = config.with_circuit_breaker(breaker);
    }
    PaymentApiGrpcClient::connect_lazy(config).unwrap()
}

/// The consumer's context: only its bearer travels, so the subject and tenant
/// set here must not matter to the provider.
fn caller(token: &str) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .bearer_token(token.to_owned())
        .build()
        .unwrap()
}

#[tokio::test]
async fn a_call_without_a_bearer_is_unauthenticated() {
    let addr = serve().await;
    let status = raw_charge(addr, None).await;
    assert_eq!(status.code(), Code::Unauthenticated);
    assert_eq!(reason(&status), "MISSING_BEARER");
}

#[tokio::test]
async fn a_malformed_authorization_is_unauthenticated() {
    let addr = serve().await;
    for value in [
        "Basic dXNlcjpwYXNz",
        "Bearer",
        "Bearer   ",
        "Bearer two tokens",
    ] {
        let status = raw_charge(addr, Some(value)).await;
        assert_eq!(status.code(), Code::Unauthenticated, "{value:?}");
        assert_eq!(reason(&status), "INVALID_BEARER", "{value:?}");
    }
}

#[tokio::test]
async fn the_scheme_is_case_insensitive_and_the_token_trimmed() {
    let addr = serve().await;
    let mut request = tonic::Request::new(stubs::ChargeRequest::from(ChargeRequest::new(
        100,
        "USD",
        "auth test",
    )));
    request
        .metadata_mut()
        .insert("authorization", "bearer  tenant-a ".parse().unwrap());
    raw_client(addr)
        .await
        .charge(request)
        .await
        .expect("accepted like the REST middleware accepts it");
}

#[tokio::test]
async fn an_invalid_token_is_unauthenticated() {
    let addr = serve().await;
    let err = PaymentApi::charge(
        &client(addr, None),
        caller("stolen"),
        ChargeRequest::new(100, "USD", "auth test"),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, CanonicalError::Unauthenticated { .. }),
        "{err:?}"
    );

    let status = raw_charge(addr, Some("Bearer stolen")).await;
    assert_eq!(reason(&status), "AUTHN_FAILED");
}

#[tokio::test]
async fn an_unexpected_backend_failure_is_internal_not_the_callers_fault() {
    let addr = serve().await;
    let status = raw_charge(addr, Some("Bearer boom")).await;
    assert_eq!(status.code(), Code::Internal);
    assert!(
        !format!("{:?}", problem(&status)).contains("plugin crashed"),
        "the backend's detail stays off the wire"
    );
}

/// The backend being down is the provider's answer, not a sign that the
/// provider is gone: the Problem keeps the consumer's breaker closed.
#[tokio::test]
async fn an_unavailable_backend_is_503_with_retry_after_and_not_counted() {
    let addr = serve().await;

    let status = raw_charge(addr, Some("Bearer down")).await;
    assert_eq!(status.code(), Code::Unavailable);
    assert_eq!(problem(&status)["context"]["retry_after_seconds"], 5);

    let breaker = Arc::new(
        CircuitBreaker::new(
            CircuitBreakerConfig::default()
                .with_minimum_calls(2)
                .with_open_for(Duration::from_secs(30)),
        )
        .unwrap(),
    );
    let client = client(addr, Some(Arc::clone(&breaker)));
    for _ in 0..5 {
        let err = PaymentApi::charge(
            &client,
            caller("down"),
            ChargeRequest::new(100, "USD", "auth test"),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, CanonicalError::ServiceUnavailable { .. }),
            "{err:?}"
        );
        assert!(
            !format!("{err:?}").contains("circuit breaker open"),
            "{err:?}"
        );
    }
    assert_eq!(breaker.state(), State::Closed);
}

/// The context the domain gets is the authenticator's: payments are listed
/// per tenant, also over a server stream.
#[tokio::test]
async fn the_authenticated_tenant_reaches_the_domain() {
    let addr = serve().await;
    let client = client(addr, None);

    let charged = PaymentApi::charge(
        &client,
        caller("tenant-a"),
        ChargeRequest::new(100, "USD", "tenant a"),
    )
    .await
    .unwrap();

    let list = |token: &'static str| {
        PaymentApi::list_payments(&client, caller(token), ListPaymentsFilter::default())
            .map(|item| item.unwrap().payment_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(list("tenant-a").await, vec![charged.payment_id]);
    assert!(list("tenant-b").await.is_empty());
}
