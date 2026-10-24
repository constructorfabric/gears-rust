//! Event Broker reaches cluster only through the SDK facades, so the same source
//! runs with cluster linked into its process or served by another one over gRPC.
//! The same cache, lock and leader-election cases run against both.
//!
//! The remote half builds a `ClientHub` with **no** cluster gear in it, only the
//! remote client the cluster SDK's wiring would register -- the process shape EB
//! has now that it no longer names `cluster` in `deps`.
//!
//! An integration test behind `server`, which links the cluster gear: both
//! halves need its wiring, and the remote half serves its gRPC services on a
//! loopback socket, which the unit suite does not.

#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use cluster::api::grpc::{
    CacheService, ClusterProfileService, DistributedLockService, ElectionSubscriptions,
    LeaderElectionService, ServiceContext,
};
use cluster::{ClusterConfig, ClusterWiring, ProfileRegistry, ProviderRegistry};
use cluster_sdk::cache::{PutRequest, Ttl};
use cluster_sdk::grpc::stubs;
use cluster_sdk::leader::{LeaderStatus, LeaderWatchEvent};
use cluster_sdk::{ClusterClient, ClusterError, RemoteClusterClient};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use toolkit::client_hub::ClientHub;

use event_broker::domain::EventBrokerCluster;

const EVENT_TIMEOUT: Duration = Duration::from_secs(5);
/// Bounds each whole scenario, so a regression fails instead of hanging.
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(30);

/// The `event-broker` profile on the standalone provider, which the cluster gear
/// also derives the lock and election from.
fn config() -> ClusterConfig {
    serde_json::from_value(serde_json::json!({
        "profiles": { "event-broker": { "cache": { "provider": "standalone" } } }
    }))
    .expect("valid test cluster config")
}

fn providers() -> ProviderRegistry {
    ProviderRegistry::new()
        .with_cache_provider(Arc::new(standalone_cluster_plugin::StandaloneCacheProvider))
}

/// The cases, written once against the facade EB actually holds.
async fn exercise(cluster: &EventBrokerCluster) {
    cluster
        .cache
        .put(PutRequest {
            key: "probe",
            value: b"linked-or-not",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    let entry = cluster
        .cache
        .get("probe")
        .await
        .expect("get")
        .expect("just written");
    assert_eq!(entry.value, b"linked-or-not");

    let guard = cluster
        .lock
        .try_lock("probe", Duration::from_secs(30))
        .await
        .expect("the lock is free");
    let contended = cluster
        .lock
        .try_lock("probe", Duration::from_secs(30))
        .await
        .expect_err("the lock is held");
    assert!(
        matches!(contended, ClusterError::LockContended { .. }),
        "expected LockContended, got: {contended}"
    );
    guard.release().await.expect("the holder can release");
    cluster
        .lock
        .try_lock("probe", Duration::from_secs(30))
        .await
        .expect("the lock is free again")
        .release()
        .await
        .expect("release");

    let mut watch = cluster
        .leader_election
        .elect("probe")
        .await
        .expect("the election is joined");
    let event = tokio::time::timeout(EVENT_TIMEOUT, watch.changed())
        .await
        .expect("an election event must arrive inside the timeout");
    assert!(
        matches!(event, LeaderWatchEvent::Status(LeaderStatus::Leader)),
        "the sole candidate must lead, got: {event:?}"
    );
    watch.resign().await.expect("the leader can step down");
}

#[tokio::test]
async fn with_cluster_linked_into_the_process() {
    // The cluster gear's own wiring in this process, as its `start` performs it:
    // wire the providers, publish the bound profiles, register the local client.
    let hub = Arc::new(ClientHub::default());
    let (mut handle, bound) = ClusterWiring::from_config(Arc::clone(&hub), &config(), &providers())
        .await
        .expect("wiring starts");
    let profiles = Arc::new(ProfileRegistry::new());
    handle.publish(&profiles, bound);

    let cluster = tokio::time::timeout(SCENARIO_TIMEOUT, EventBrokerCluster::resolve(&hub))
        .await
        .expect("the profile resolves inside the timeout")
        .expect("the event-broker profile is bound");
    tokio::time::timeout(SCENARIO_TIMEOUT, exercise(&cluster))
        .await
        .expect("the scenario finishes inside the timeout");
    handle.stop().await;
}

#[tokio::test]
async fn with_cluster_served_by_another_process() {
    // The other process: the cluster gear's four services over a real socket.
    let (handle, bound) =
        ClusterWiring::from_config(Arc::new(ClientHub::new()), &config(), &providers())
            .await
            .expect("wiring starts");
    let registry = Arc::new(ProfileRegistry::new());
    registry.publish(bound);
    let ctx = ServiceContext::new(Arc::clone(&registry));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let addr = listener.local_addr().expect("has an address");
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        Server::builder()
            .add_service(
                stubs::cache::cluster_cache_api_server::ClusterCacheApiServer::new(
                    CacheService::new(ctx.clone()),
                ),
            )
            .add_service(
                stubs::lock::distributed_lock_api_server::DistributedLockApiServer::new(
                    DistributedLockService::new(ctx.clone()),
                ),
            )
            .add_service(
                stubs::leader::leader_election_api_server::LeaderElectionApiServer::new(
                    LeaderElectionService::new(ctx.clone(), Arc::new(ElectionSubscriptions::new())),
                ),
            )
            .add_service(
                stubs::profile::cluster_profile_api_server::ClusterProfileApiServer::new(
                    ClusterProfileService::new(ctx),
                ),
            )
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _stopped = shutdown_rx.await;
            })
            .await
            .expect("the server runs");
    });

    // EB's process: no cluster gear, only the remote client.
    let hub = ClientHub::default();
    let client = RemoteClusterClient::connect_lazy(&format!("http://{addr}"), None)
        .expect("a valid endpoint");
    hub.register::<dyn ClusterClient>(Arc::new(client));

    let cluster = tokio::time::timeout(SCENARIO_TIMEOUT, EventBrokerCluster::resolve(&hub))
        .await
        .expect("the profile resolves inside the timeout")
        .expect("the event-broker profile resolves over the wire");
    tokio::time::timeout(SCENARIO_TIMEOUT, exercise(&cluster))
        .await
        .expect("the scenario finishes inside the timeout");

    let _sent = shutdown.send(());
    tokio::time::timeout(EVENT_TIMEOUT, server)
        .await
        .expect("the server stops inside the timeout")
        .expect("the server stops");
    handle.stop().await;
}
