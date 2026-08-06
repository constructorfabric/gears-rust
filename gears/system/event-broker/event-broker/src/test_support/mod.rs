//! Shared test fixtures (`DESIGN.md:614`) - real cluster-mode wiring for
//! dispatcher tests (`eb-dispatcher-routing`), not the comment-only shell
//! `gears-rust#4427` removed.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use cluster::{ClusterConfig, ClusterHandle, ClusterWiring, ProviderRegistry};
use cluster_sdk::{ServiceHandle, ServiceRegistration};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;

use crate::domain::cluster::EventBrokerCluster;

/// Wires a `standalone` cache provider under the `event-broker` cluster
/// profile (matching `domain::cluster::EventBrokerCluster::resolve()`'s
/// profile name) and resolves an `EventBrokerCluster` against it.
///
/// The returned `ClientHub` is the same one `EventBrokerCluster` resolved
/// against, so a test's own `register()`/`discover()` calls observe the
/// same underlying state. The `ClusterHandle` is intentionally leaked
/// (`ClusterHandle::drop` without `stop()` panics in debug builds by
/// design, as a production safety net - tests don't need graceful
/// shutdown, the process ends shortly after).
pub async fn standalone_event_broker_cluster() -> (Arc<ClientHub>, EventBrokerCluster) {
    let hub = Arc::new(ClientHub::default());
    let config: ClusterConfig = serde_json::from_value(serde_json::json!({
        "profiles": { "event-broker": { "cache": { "provider": "standalone" } } }
    }))
    .expect("valid test cluster config");
    let providers = ProviderRegistry::new()
        .with_cache_provider(Arc::new(standalone_cluster_plugin::StandaloneCacheProvider));

    let handle = ClusterWiring::from_config(Arc::clone(&hub), &config, &providers)
        .await
        .expect("standalone provider wires cleanly");
    forget_handle(handle);

    let cluster = EventBrokerCluster::resolve(&hub).expect("event-broker profile is bound");
    (hub, cluster)
}

fn forget_handle(handle: ClusterHandle) {
    std::mem::forget(handle);
}

/// A running mock ingest/delivery instance: a real axum server on an
/// ephemeral local port, registered with `ServiceDiscoveryV1` under
/// `service_name`. Dropping this stops the server and lets the registration
/// lapse via TTL (no explicit deregister - tests don't need graceful
/// shutdown).
pub struct MockInstance {
    #[allow(
        dead_code,
        reason = "public fixture field - not every test needs to inspect the addr"
    )]
    pub addr: SocketAddr,
    server: JoinHandle<()>,
    shutdown: CancellationToken,
    _registration: ServiceHandle,
}

impl Drop for MockInstance {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.server.abort();
    }
}

/// Binds `router` to an ephemeral local port, serves it in the background,
/// and registers the bound address with `cluster.service_discovery` under
/// `service_name` (`"http://127.0.0.1:{port}"` - the same `ServiceInstance`
/// shape `forward()` resolves against, design.md D5).
pub async fn mock_instance(
    cluster: &EventBrokerCluster,
    service_name: &str,
    router: axum::Router,
) -> MockInstance {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binding an ephemeral local port must not fail");
    let addr = listener
        .local_addr()
        .expect("a just-bound listener has a local address");

    let shutdown = CancellationToken::new();
    let shutdown_signal = shutdown.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown_signal.cancelled().await })
            .await
            .expect("mock instance server must not error");
    });

    let registration = cluster
        .service_discovery
        .register(ServiceRegistration {
            name: service_name.to_owned(),
            instance_id: None,
            address: format!("http://{addr}"),
            metadata: HashMap::new(),
        })
        .await
        .expect("registering the mock instance must not fail");

    MockInstance {
        addr,
        server,
        shutdown,
        _registration: registration,
    }
}
