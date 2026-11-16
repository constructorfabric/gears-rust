//! Shared test fixtures (`DESIGN.md:614`) - real cluster-mode wiring for
//! dispatcher tests (`eb-dispatcher-routing`), not the comment-only shell
//! `gears-rust#4427` removed. `harness`/`api_v1`/`request`/`response`/`body`
//! (`eb-rest-handlers`) are the HTTP integration-test harness for this
//! crate's own ingest/delivery REST handlers - a separate concern from the
//! dispatcher fixtures below, file-for-file mirroring `oagw`'s
//! `test_support/` (design.md "Test harness follows oagw's two-layer
//! `test_support` pattern").

// `test_support` is test scaffolding exposed to other crates' tests via the
// `test-utils` feature. Under that feature it compiles as non-`test` code, so
// the restriction/pedantic lints clippy waives inside `#[cfg(test)]` -
// `expect`/`panic` in setup, `# Panics` docs, must-use getters - fire here.
// They are noise on a test harness, so waive them for this module tree only;
// production code keeps the workspace defaults.
#![allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use
)]

pub mod api_v1;
pub mod authz_doubles;
pub mod body;
pub mod fault_broker;
pub mod gear_db;
// Not every harness convenience method (builder customization beyond
// tenant/subject id, additional response assertions like `assert_header`)
// is exercised by the specific handler tests that exist today (task group
// 11) - a general-purpose harness API surface, mirroring oagw's, is
// allowed to have unused corners rather than being trimmed to exactly what
// today's tests happened to need.
#[allow(
    dead_code,
    reason = "general-purpose harness API surface, not all of it exercised yet"
)]
pub mod harness;
pub mod log_capture;
pub mod module_boot;
#[allow(
    dead_code,
    reason = "general-purpose harness API surface, not all of it exercised yet"
)]
pub mod never_storing_backend;
#[allow(
    dead_code,
    reason = "general-purpose harness API surface, not all of it exercised yet"
)]
pub mod outbox_rows;
pub mod pause;
#[allow(
    dead_code,
    reason = "general-purpose harness API surface, not all of it exercised yet"
)]
pub mod request;
pub mod response;
pub mod single_backend;
pub mod tables;
pub mod type_registry;

pub use authz_doubles::DenyingAuthZ;
#[allow(
    unused_imports,
    reason = "general-purpose harness API surface, not all of it exercised yet"
)]
pub use body::IntoBody;
pub use body::Json;
pub use fault_broker::{DeliveredOutcome, DeliveredPublish, FaultInjectingBroker, TransientFault};
pub use gear_db::GearDb;
#[allow(
    unused_imports,
    reason = "general-purpose harness API surface, not all of it exercised yet"
)]
pub use harness::{EventBrokerHarness, EventBrokerHarnessBuilder};
pub use log_capture::mapping_lines;
#[allow(
    unused_imports,
    reason = "general-purpose harness API surface, not all of it exercised yet"
)]
pub use never_storing_backend::NeverStoringBackend;
pub use outbox_rows::outbox_queue_partitions;
pub use pause::PausePoint;
#[allow(
    unused_imports,
    reason = "general-purpose harness API surface, not all of it exercised yet"
)]
pub use request::RequestCase;
#[allow(
    unused_imports,
    reason = "general-purpose harness API surface, not all of it exercised yet"
)]
pub use response::TestResponse;
pub use single_backend::SingleBackendResolver;
pub use type_registry::StaticTypesRegistry;

/// The GTS **type** identifier an event names, for a test that builds an
/// `Event` directly. A concrete event type is a derived type schema, so the
/// identifier ends in `~` and `GtsInstanceId` would refuse it.
///
/// # Panics
/// Panics if `raw` is not a well-formed GTS type identifier - a fixture
/// mistake, not a runtime condition.
#[must_use]
pub fn event_type_id(raw: &str) -> gts::GtsTypeId {
    gts::GtsTypeId::try_new(raw)
        .unwrap_or_else(|err| panic!("'{raw}' is not a valid GTS type id: {err}"))
}

use std::net::SocketAddr;
use std::sync::Arc;

use cluster_sdk::ClusterCacheProvider as _;
use cluster_sdk::test_support::{StubClusterClient, unbound_leader_election, unbound_lock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;
use toolkit::directory::{DirectoryClient, RegisterInstanceInfo, ServiceEndpoint};
use toolkit::runtime::GearManager;
use uuid::Uuid;

use crate::domain::cluster::EventBrokerCluster;

/// `standalone_event_broker_cluster()` wires the toolkit's own real,
/// production `LocalDirectoryClient` (backed by a fresh `GearManager`, the
/// same in-memory store `service-discovery` itself runs standalone mode on -
/// no DB, no network) into the same `ClientHub` production code paths
/// (`module::EventBrokerModule::register_self`, `infra::dispatcher::forward`)
/// resolve their `Arc<dyn DirectoryClient>` from, so dispatcher tests
/// exercise the real directory-client behavior (round-robin, instance
/// state) rather than a second, independently-maintained mock of it.
///
/// Fetches the `LocalDirectoryClient` `standalone_event_broker_cluster()`
/// registered into `hub`, typed as the trait object production code
/// resolves.
pub fn test_directory_client(hub: &ClientHub) -> Arc<dyn DirectoryClient> {
    hub.get::<dyn DirectoryClient>()
        .expect("standalone_event_broker_cluster() registers a LocalDirectoryClient")
}

/// The cluster profile the broker binds to, matching the private
/// `EventBrokerProfile::NAME` in `domain::cluster`. The harness binds the stub
/// under the same name so `EventBrokerCluster::resolve()` finds it.
const EVENT_BROKER_PROFILE: &str = "event-broker";

/// Binds the `event-broker` cluster profile into `hub` through `cluster-sdk`'s
/// [`StubClusterClient`], over a `standalone-cluster-plugin` in-memory cache -
/// the way a consumer's tests resolve a cluster facade without linking the
/// cluster gear (same pattern as `examples/toolkit/api-contracts`'s
/// `cluster_round_trip`).
///
/// Only the cache primitive is bound; the lock and leader-election primitives
/// are left unbound (resolution succeeds, use would fail) because no
/// event-broker test exercises them. The cache's background TTL sweeper
/// (`stop`) is intentionally dropped rather than stopped: tests need no
/// graceful shutdown and the process ends shortly after.
pub async fn register_standalone_cluster_stub(hub: &ClientHub) {
    let (cache, _stop) = standalone_cluster_plugin::StandaloneCacheProvider
        .build_cache(&serde_json::Map::new())
        .await
        .expect("the standalone cache builds");
    StubClusterClient::for_profile(EVENT_BROKER_PROFILE)
        .with_cache(cache)
        .with_lock(unbound_lock(EVENT_BROKER_PROFILE))
        .with_leader_election(unbound_leader_election(EVENT_BROKER_PROFILE))
        .register(hub);
}

/// Binds the `event-broker` cluster profile (via
/// [`register_standalone_cluster_stub`]) and resolves an `EventBrokerCluster`
/// against it.
///
/// The returned `ClientHub` is the same one `EventBrokerCluster` resolved
/// against, so a test's own `register()`/`discover()` calls observe the
/// same underlying state.
pub async fn standalone_event_broker_cluster() -> (Arc<ClientHub>, EventBrokerCluster) {
    let hub = Arc::new(ClientHub::default());
    hub.register::<dyn DirectoryClient>(Arc::new(toolkit::directory::LocalDirectoryClient::new(
        Arc::new(GearManager::new()),
    )));
    register_standalone_cluster_stub(&hub).await;

    let cluster = EventBrokerCluster::resolve(&hub)
        .await
        .expect("event-broker profile is bound");
    (hub, cluster)
}

/// A running mock ingest/delivery instance: a real axum server on an
/// ephemeral local port, registered with `DirectoryService` under
/// `gear_name`. Dropping this stops the server; the directory entry is not
/// explicitly deregistered (each test builds a fresh `ClientHub`/directory,
/// so there's no cross-test leakage, and tests don't need graceful
/// shutdown).
pub struct MockInstance {
    #[allow(
        dead_code,
        reason = "public fixture field - not every test needs to inspect the addr"
    )]
    pub addr: SocketAddr,
    server: JoinHandle<()>,
    shutdown: CancellationToken,
}

impl Drop for MockInstance {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.server.abort();
    }
}

/// Binds `router` to an ephemeral local port, serves it in the background,
/// and registers the bound address with `directory` under `gear_name`
/// (`"http://127.0.0.1:{port}"` - the same `ServiceInstanceInfo` shape
/// `forward()` resolves against, design.md D5).
pub async fn mock_instance(
    directory: &Arc<dyn DirectoryClient>,
    gear_name: &str,
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

    directory
        .register_instance(
            RegisterInstanceInfo::new(gear_name.to_owned(), Uuid::new_v4().to_string())
                .with_rest_endpoint(ServiceEndpoint::new(format!("http://{addr}"))),
        )
        .await
        .expect("registering the mock instance must not fail");

    MockInstance {
        addr,
        server,
        shutdown,
    }
}
