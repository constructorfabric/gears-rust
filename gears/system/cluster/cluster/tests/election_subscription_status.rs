// Created: 2026-08-13 by Constructor Tech
//! `SEAM-5` repro — the status an election subscription really raises, decoded
//! with the lease context that names it.
//!
//! `AwaitChange` addresses a *subscription*, which is replica-local, so the
//! server answers a bare `NotFound` whenever the replica serving it went away
//! (`api/grpc/leader.rs`, `Status::not_found("unknown election_id")`). Section
//! 6.9's table maps that row to `Closed(ClusterError::Shutdown)` — terminal, so
//! `RestartingWatch` propagates rather than resubscribing.
//!
//! The premise has to be checked against a real server rather than a hand-built
//! `Status`: the whole finding is that the server's answer carries **no** cluster
//! error envelope, so the decode depends entirely on the `LeaseContext` the
//! caller passes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

use std::sync::Arc;

use cluster::api::grpc::{
    CallerResolver, ElectionSubscriptions, LeaderElectionService, ServiceContext,
};
use cluster::{ClusterConfig, ClusterWiring, ProfileRegistry, ProviderRegistry};
use cluster_sdk::grpc::stubs;
use cluster_sdk::{ClusterError, LeaseContext, from_lease_status, from_status};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use toolkit::client_hub::ClientHub;

const PROFILE: &str = "orders";

/// Asks a real gear to `await_change` on an election it never issued, and hands
/// back the `Status` it answered with.
async fn unknown_subscription_status() -> tonic::Status {
    let cfg: ClusterConfig =
        serde_saphyr::from_str("profiles:\n  orders:\n    cache: { provider: standalone }\n")
            .expect("config parses");
    let providers = ProviderRegistry::new()
        .with_cache_provider(Arc::new(standalone_cluster_plugin::StandaloneCacheProvider));
    let (handle, bound) = ClusterWiring::from_config(Arc::new(ClientHub::new()), &cfg, &providers)
        .await
        .expect("wiring starts");

    let registry = Arc::new(ProfileRegistry::new());
    registry.publish(bound);
    let ctx = ServiceContext::new(Arc::clone(&registry), CallerResolver::trusted_network());

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let addr = listener.local_addr().expect("has an address");
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        Server::builder()
            .add_service(
                stubs::leader::leader_election_api_server::LeaderElectionApiServer::new(
                    LeaderElectionService::new(ctx, Arc::new(ElectionSubscriptions::new())),
                ),
            )
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _stopped = shutdown_rx.await;
            })
            .await
            .expect("the server runs");
    });

    let mut raw = stubs::leader::leader_election_api_client::LeaderElectionApiClient::connect(
        format!("http://{addr}"),
    )
    .await
    .expect("connects");

    let status = raw
        .await_change(stubs::leader::AwaitChangeRequest {
            profile: PROFILE.to_owned(),
            election_id: "an-election-this-replica-never-issued".to_owned(),
        })
        .await
        .expect_err("an unknown election_id must not open a stream");

    let _sent = shutdown.send(());
    handle.stop().await;
    status
}

/// The finding, end to end. The same real status decodes two different ways, and
/// only one of them is the answer section 6.9 specifies.
#[tokio::test]
async fn a_subscription_notfound_decodes_as_shutdown_only_with_its_lease_context() {
    let status = unknown_subscription_status().await;

    // The premise: a bare `NotFound`, with no cluster error envelope for the
    // codec to key on. If this ever stops holding, the finding is moot and this
    // test is what says so.
    assert_eq!(status.code(), tonic::Code::NotFound, "{status:?}");
    assert!(
        status.metadata().get_bin("x-toolkit-problem-bin").is_none(),
        "the server types no cluster error here - that is what makes the LeaseContext \
         load-bearing"
    );

    // What the two call sites did before the fix: `from_status`, i.e.
    // `LeaseContext::None`.
    let without_context = from_status(&status);
    assert!(
        matches!(
            without_context,
            ClusterError::Provider {
                kind: cluster_sdk::ProviderErrorKind::Other,
                ..
            }
        ),
        "pinning the pre-fix behaviour, got {without_context:?}"
    );

    // What section 6.9's table specifies, and what the fix passes.
    let with_context = from_lease_status(&status, LeaseContext::ElectionSubscription)
        .expect("a subscription absence is an error, not release-by-absence");
    assert!(
        matches!(with_context, ClusterError::Shutdown),
        "a subscription whose replica went away is a terminal shutdown, got {with_context:?}"
    );

    // Retryability agrees either way - which is why this is minor, and why the
    // consumer-visible variant is the whole of the difference.
    assert!(!without_context.is_retryable());
    assert!(!with_context.is_retryable());
}
