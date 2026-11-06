//! `PaymentApiGrpcResolvingClient` against real tonic servers found through
//! the in-process directory: eventual readiness, a provider that moves, the
//! circuit breaker opening and recovering, and versioned service names.

#![cfg(feature = "grpc-client")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::grpc::{
    PAYMENT_API_GRPC_SERVICE, PaymentApiGrpcClient, PaymentApiGrpcResolvingClient,
};
use api_contracts_sdk::models::ChargeRequest;
use tokio::sync::oneshot;
use toolkit::directory::LocalDirectoryClient;
use toolkit::discovery::{DirectoryEndpointResolver, EndpointResolver};
use toolkit::runtime::{Endpoint, GearInstance, GearManager};
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::{ClientConfig, RetryConfig};
use toolkit_contract::wiring::{ClientTuning, RetrySettings};
use toolkit_resilience::circuit_breaker::CircuitBreakerConfig;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use cf_api_contracts::api::grpc::PaymentApiGrpcService;
use cf_api_contracts::domain::service::PaymentDomainService;

const PROVIDER: &str = "payment-provider";

struct Server {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    done: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        drop(tokio::time::timeout(Duration::from_secs(5), self.done).await);
    }
}

/// Serve `PaymentApi` on `addr` (`127.0.0.1:0` for any port).
async fn serve(addr: SocketAddr) -> Server {
    let svc = PaymentApiGrpcService::new(Arc::new(PaymentDomainService::new())).into_server();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();
    let done = tokio::spawn(async move {
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
    Server {
        addr,
        stop: Some(tx),
        done,
    }
}

fn any_port() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// Publish (or move) the provider's single instance to `addr`.
fn publish(gears: &GearManager, instance: Uuid, addr: SocketAddr) {
    gears
        .register_instance(Arc::new(
            GearInstance::new(PROVIDER, instance).with_grpc_service(
                PAYMENT_API_GRPC_SERVICE,
                Endpoint::from_uri(format!("http://{addr}")),
            ),
        ))
        .unwrap();
}

fn tuning(breaker: CircuitBreakerConfig) -> ClientTuning {
    ClientTuning {
        timeout: Some(Duration::from_secs(2)),
        retry: Some(RetrySettings {
            max_attempts: Some(1),
            ..RetrySettings::default()
        }),
        circuit_breaker: Some(breaker),
        ..ClientTuning::default()
    }
}

fn consumer(
    gears: &Arc<GearManager>,
    breaker: CircuitBreakerConfig,
) -> PaymentApiGrpcResolvingClient {
    let resolver =
        DirectoryEndpointResolver::new(Arc::new(LocalDirectoryClient::new(Arc::clone(gears))));
    PaymentApiGrpcResolvingClient::new(Arc::new(resolver), PROVIDER, tuning(breaker))
}

async fn charge(client: &dyn PaymentApi) -> Result<(), CanonicalError> {
    client
        .charge(
            SecurityContext::anonymous(),
            ChargeRequest::new(100, "USD", "test"),
        )
        .await
        .map(drop)
}

/// Retry `charge` until it succeeds, for up to `within`.
async fn eventually_ok(client: &dyn PaymentApi, within: Duration) -> bool {
    tokio::time::timeout(within, async {
        while charge(client).await.is_err() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn the_client_waits_for_the_provider_and_then_calls_it() {
    let gears = Arc::new(GearManager::new());
    let client = consumer(&gears, CircuitBreakerConfig::default());

    let err = charge(&client).await.unwrap_err();
    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "{err:?}"
    );

    let server = serve(any_port()).await;
    publish(&gears, Uuid::new_v4(), server.addr);
    charge(&client).await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn the_client_follows_a_provider_that_moved() {
    let gears = Arc::new(GearManager::new());
    let instance = Uuid::new_v4();
    let client = consumer(&gears, CircuitBreakerConfig::disabled());

    let first = serve(any_port()).await;
    publish(&gears, instance, first.addr);
    charge(&client).await.unwrap();

    let second = serve(any_port()).await;
    publish(&gears, instance, second.addr);
    first.stop().await;

    // The failed call against the old address drops the cached lookup, so the
    // client moves without waiting out the resolver's TTL.
    assert!(eventually_ok(&client, Duration::from_secs(5)).await);
    second.stop().await;
}

#[tokio::test]
async fn a_dead_provider_opens_the_breaker_and_it_closes_after_recovery() {
    let gears = Arc::new(GearManager::new());
    let server = serve(any_port()).await;
    let addr = server.addr;
    publish(&gears, Uuid::new_v4(), addr);
    let client = consumer(
        &gears,
        CircuitBreakerConfig::default()
            .with_minimum_calls(2)
            .with_open_for(Duration::from_millis(500)),
    );
    charge(&client).await.unwrap();
    server.stop().await;

    for _ in 0..2 {
        let err = charge(&client).await.unwrap_err();
        assert!(
            matches!(err, CanonicalError::ServiceUnavailable { .. }),
            "{err:?}"
        );
    }
    let refused = charge(&client).await.unwrap_err();
    assert!(
        format!("{refused:?}").contains("circuit breaker open"),
        "the breaker refuses without calling: {refused:?}"
    );

    // Same address comes back: after `open_for` a probe goes through and
    // closes the breaker.
    let revived = serve(addr).await;
    assert!(eventually_ok(&client, Duration::from_secs(5)).await);
    revived.stop().await;
}

#[tokio::test]
async fn each_major_version_resolves_to_its_own_provider() {
    let gears = Arc::new(GearManager::new());
    let v2 = PAYMENT_API_GRPC_SERVICE.replace(".v1.", ".v2.");
    for (gear, service, port) in [
        ("payments-v1", PAYMENT_API_GRPC_SERVICE, 50061),
        ("payments-v2", v2.as_str(), 50062),
    ] {
        gears
            .register_instance(Arc::new(
                GearInstance::new(gear, Uuid::new_v4())
                    .with_grpc_service(service, Endpoint::http("127.0.0.1", port)),
            ))
            .unwrap();
    }
    let resolver = DirectoryEndpointResolver::new(Arc::new(LocalDirectoryClient::new(gears)));

    assert_eq!(
        resolver
            .resolve_grpc_endpoint(PAYMENT_API_GRPC_SERVICE)
            .await
            .unwrap()
            .as_deref(),
        Some("http://127.0.0.1:50061")
    );
    assert_eq!(
        resolver
            .resolve_grpc_endpoint(&v2)
            .await
            .unwrap()
            .as_deref(),
        Some("http://127.0.0.1:50062")
    );
}

/// A provider on an older minor version lacks a newer RPC: the caller gets
/// `Unimplemented`, not an opaque internal error.
#[tokio::test]
async fn a_missing_rpc_is_reported_as_unimplemented() {
    // A server with no `PaymentApi` service answers every call UNIMPLEMENTED.
    let listener = tokio::net::TcpListener::bind(any_port()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        drop(
            tonic::transport::Server::builder()
                .add_routes(tonic::service::Routes::default())
                .serve_with_incoming(incoming)
                .await,
        );
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = PaymentApiGrpcClient::connect_lazy(
        ClientConfig::new(format!("http://{addr}"))
            .with_timeout(Duration::from_secs(2))
            .with_retry(RetryConfig::off()),
    )
    .unwrap();
    let err = charge(&client).await.unwrap_err();
    assert!(
        matches!(err, CanonicalError::Unimplemented { .. }),
        "{err:?}"
    );
}

/// The generated service name is what tonic routes and registers under.
#[test]
fn the_service_name_constant_matches_tonic() {
    use tonic::server::NamedService;
    type Server =
        api_contracts_sdk::grpc::stubs::payment_api_server::PaymentApiServer<PaymentApiGrpcService>;
    assert_eq!(PAYMENT_API_GRPC_SERVICE, <Server as NamedService>::NAME);
    assert_eq!(
        PaymentApiGrpcResolvingClient::GRPC_SERVICE,
        PAYMENT_API_GRPC_SERVICE
    );
}
