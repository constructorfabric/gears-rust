// Created: 2026-08-13 by Constructor Tech
//! `ERR-1` / `SEAM-4` repro — the error codec over a **real** gRPC hop.
//!
//! An in-process conversion test cannot see any of this: the defect lives in the
//! interaction between `attach_problem`'s trailer cap and `from_lease_status`'s
//! catch-all, and only a real tonic server over TCP puts both in the path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]
#![allow(
    clippy::use_debug,
    reason = "this file's value is its printed evidence: the decoded variant, the gRPC code and \
              the raw trailer are dumped verbatim so the repro output can be read against the \
              audit's"
)]

use std::sync::Arc;

use cluster::api::grpc::{
    CacheService, CallerResolver, ClusterProfileService, DistributedLockService,
    ElectionSubscriptions, LeaderElectionService, ServiceContext,
};
use cluster::{ClusterConfig, ClusterHandle, ClusterWiring, ProfileRegistry, ProviderRegistry};
use cluster_sdk::cache::{PutRequest, Ttl};
use cluster_sdk::grpc::stubs;
use cluster_sdk::{ClusterClient, ClusterError, RemoteClusterClient};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use toolkit::client_hub::ClientHub;

const PROFILE: &str = "orders";

struct Fixture {
    client: RemoteClusterClient,
    endpoint: String,
    handle: ClusterHandle,
    shutdown: tokio::sync::oneshot::Sender<()>,
}

impl Fixture {
    /// `serve_cache` off stands the gear up with **no** cache service routed, so
    /// tonic itself answers `Unimplemented` — the version-skew shape section 6.11
    /// names.
    async fn start(serve_cache: bool) -> Self {
        let cfg: ClusterConfig =
            serde_saphyr::from_str("profiles:\n  orders:\n    cache: { provider: standalone }\n")
                .expect("config parses");
        let providers = ProviderRegistry::new()
            .with_cache_provider(Arc::new(standalone_cluster_plugin::StandaloneCacheProvider));
        let (handle, bound) =
            ClusterWiring::from_config(Arc::new(ClientHub::new()), &cfg, &providers)
                .await
                .expect("wiring starts");

        let registry = Arc::new(ProfileRegistry::new());
        registry.publish(bound);

        let ctx = ServiceContext::new(Arc::clone(&registry), CallerResolver::trusted_network());
        let subscriptions = Arc::new(ElectionSubscriptions::new());

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
        let addr = listener.local_addr().expect("has an address");
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let mut router = Server::builder().add_service(
                stubs::lock::distributed_lock_api_server::DistributedLockApiServer::new(
                    DistributedLockService::new(ctx.clone()),
                ),
            );
            if serve_cache {
                router = router.add_service(
                    stubs::cache::cluster_cache_api_server::ClusterCacheApiServer::new(
                        CacheService::new(ctx.clone()),
                    ),
                );
            }
            router
                .add_service(
                    stubs::leader::leader_election_api_server::LeaderElectionApiServer::new(
                        LeaderElectionService::new(ctx.clone(), subscriptions),
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

        let endpoint = format!("http://{addr}");
        let client = RemoteClusterClient::connect_lazy(&endpoint).expect("a valid endpoint");

        Self {
            client,
            endpoint,
            handle,
            shutdown,
        }
    }

    fn cache(&self) -> Arc<dyn cluster_sdk::ClusterCacheBackend> {
        self.client.cache_backend(PROFILE).expect("a handle")
    }

    async fn stop(self) {
        let _sent = self.shutdown.send(());
        self.handle.stop().await;
    }
}

/// Names the decoded variant compactly enough to print one line per size.
fn name(error: &ClusterError) -> String {
    match error {
        ClusterError::CasConflict { current, .. } => {
            format!(
                "CasConflict{{current: {}}}",
                if current.is_some() { "some" } else { "none" }
            )
        }
        ClusterError::Provider { kind, .. } => format!("Provider{{{kind:?}}}"),
        other => format!("{other:?}")
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned(),
    }
}

/// **The `ERR-1` repro.** Walks the CAS value size across the boundary the
/// trailer cap plus `ERR-2`'s ~4x integer-array encoding puts it at, and asserts
/// the one property section 6.10 depends on: a CAS conflict is **never**
/// retryable, at any payload size.
///
/// Before the fix the 953-byte row and every row above it decoded as
/// `Provider{ConnectionLost}` with `retryable = true`.
#[tokio::test]
async fn oversized_cas_conflict_over_a_real_grpc_hop() {
    let fixture = Fixture::start(true).await;
    let cache = fixture.cache();

    let mut failures = Vec::new();
    for size in [100_usize, 953, 954, 2048, 8192] {
        let key = format!("ledger-{size}");
        let value = vec![b'x'; size];

        cache
            .put(PutRequest {
                key: &key,
                value: &value,
                ttl: Ttl::Indefinite,
            })
            .await
            .expect("the seed put succeeds");

        // Version 1 is current, so version 99 is stale: a real conflict, whose
        // `current` the standalone plugin populates with the full entry.
        let conflict = cache
            .compare_and_swap(&key, 99, b"new", Ttl::Indefinite)
            .await
            .expect_err("a stale version must conflict");

        println!(
            "cas value {size:>5}B -> {:<28} retryable {}",
            name(&conflict),
            conflict.is_retryable()
        );

        if conflict.is_retryable() {
            failures.push(format!("{size}B decoded retryable: {conflict:?}"));
        }
        if !matches!(conflict, ClusterError::CasConflict { .. }) {
            failures.push(format!(
                "{size}B lost the CasConflict variant: {conflict:?}"
            ));
        }
    }

    fixture.stop().await;
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// **The `SEAM-4` repro.** A method the server does not serve is section 6.11's
/// rolling-deployment skew, and the design's table maps
/// `Unimplemented <-> Unsupported <-> not retryable`. Classified retryable, the
/// default `RetryPolicy` (`max_retries: None`) resubscribes forever.
#[tokio::test]
async fn an_unimplemented_method_over_a_real_grpc_hop() {
    let fixture = Fixture::start(false).await;
    let cache = fixture.cache();

    let error = cache
        .get("anything")
        .await
        .expect_err("the cache service is not routed");

    println!("unimplemented -> {error:?}");
    println!("            retryable = {}", error.is_retryable());

    let retryable = error.is_retryable();
    fixture.stop().await;
    assert!(
        !retryable,
        "an `Unimplemented` is a permanent version skew, not a transient loss"
    );
}

/// The evidence the cluster-side fix rests on: `x-toolkit-problem-truncated`
/// really does reach the client through a real tonic hop, so the decoder can
/// tell "the server answered and its answer did not fit" from "the channel is
/// gone".
///
/// It also shows the encode-side fix working: after it, a 4096-byte conflict
/// arrives **untruncated and parseable**, because the server shed
/// `current_value` rather than letting `attach_problem` shed the `error_code`.
#[tokio::test]
async fn the_truncated_header_survives_a_real_grpc_hop() {
    use stubs::cache::cluster_cache_api_client::ClusterCacheApiClient;

    let fixture = Fixture::start(true).await;
    let mut raw = ClusterCacheApiClient::connect(fixture.endpoint.clone())
        .await
        .expect("connects");

    for size in [100_usize, 4096] {
        let key = format!("probe-{size}");
        raw.put(stubs::cache::PutRequest {
            profile: PROFILE.to_owned(),
            key: key.clone(),
            value: vec![b'x'; size],
            ttl_ms: None,
            client_request_id: None,
        })
        .await
        .expect("the seed put succeeds");

        let status = raw
            .compare_and_swap(stubs::cache::CasRequest {
                profile: PROFILE.to_owned(),
                key: key.clone(),
                expected_version: 99,
                new_value: b"new".to_vec(),
                ttl_ms: None,
            })
            .await
            .expect_err("a stale version must conflict");

        let trailer = status
            .metadata()
            .get_bin("x-toolkit-problem-bin")
            .map(|raw| raw.to_bytes().expect("base64 decodes"));
        let truncated = status
            .metadata()
            .get("x-toolkit-problem-truncated")
            .map(|value| value.to_str().expect("ascii").to_owned());

        println!("--- cas value {size} bytes ---");
        println!("  code               = {:?}", status.code());
        println!("  trailer present    = {}", trailer.is_some());
        println!(
            "  trailer bytes      = {}",
            trailer.as_ref().map_or(0, tonic::codegen::Bytes::len)
        );
        println!("  TRUNCATED HEADER   = {truncated:?}");
        if let Some(bytes) = &trailer {
            let text = String::from_utf8_lossy(bytes);
            println!("  trailer body       = {text}");
            let parsed = serde_json::from_slice::<toolkit_canonical_errors::Problem>(bytes)
                .map_or_else(|error| error.to_string(), |_| "yes".to_owned());
            println!("  parses as Problem? = {parsed}");
        }
    }

    fixture.stop().await;
}
