//! A gear consuming `PaymentApi` over gRPC at an address pinned with
//! `gears.<gear>.clients.consumes.<dep>.grpc_endpoint`: the full `HostRuntime`
//! phases, no directory at all, and a real call to a tonic server.
//!
//! A test binary of its own: `inventory` collects every `#[consumes]` linked
//! into the binary, and this one must be the only consumer here.

#![cfg(feature = "grpc-client")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::sync::Arc;
use std::time::Duration;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::models::{ChargeRequest, PaymentStatus};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use toolkit::config::ConfigProvider;
use toolkit::contracts::Gear;
use toolkit::registry::RegistryBuilder;
use toolkit::runtime::HostRuntime;
use toolkit::{ClientHub, DbOptions, DependencyChecker, GearCtx};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use cf_api_contracts::api::grpc::PaymentApiGrpcService;
use cf_api_contracts::domain::service::PaymentDomainService;

const CONSUMER_GEAR: &str = "pinned-consumer";

#[toolkit::consumes(
    contract = api_contracts_sdk::PaymentApi,
    from = "payment-provider",
    transport = grpc
)]
struct PinnedConsumer;

struct Plain;

#[async_trait]
impl Gear for Plain {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

struct PinnedConfig(serde_json::Value);

impl ConfigProvider for PinnedConfig {
    fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
        self.0.get(gear_name)
    }
}

async fn serve_provider() -> String {
    let svc = PaymentApiGrpcService::new(Arc::new(PaymentDomainService::new())).into_server();
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
    format!("http://{addr}")
}

#[tokio::test]
async fn a_pinned_grpc_endpoint_needs_no_directory() {
    let _ = PinnedConsumer;
    let endpoint = serve_provider().await;
    let config = serde_json::json!({
        CONSUMER_GEAR: { "clients": { "consumes": { "payment-provider": {
            "grpc_endpoint": endpoint,
            "require_tls": false
        } } } }
    });

    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta(CONSUMER_GEAR, &[], Arc::new(Plain));
    let registry = builder.build_topo_sorted().expect("registry build");

    // No `DirectoryClient` in the hub: nothing could be discovered.
    let hub = Arc::new(ClientHub::new());
    let cancel = CancellationToken::new();
    let runtime = HostRuntime::new(
        registry,
        Arc::new(PinnedConfig(config)),
        DbOptions::None,
        Arc::clone(&hub),
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );
    let run = tokio::spawn(async move { runtime.run_gear_phases().await });

    let client = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(client) = hub.for_gear(CONSUMER_GEAR).get::<dyn PaymentApi>() {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the consumer's client is wired");

    let resp = client
        .charge(
            SecurityContext::anonymous(),
            ChargeRequest::new(500, "USD", "pinned"),
        )
        .await
        .expect("the call reaches the pinned provider");
    assert_eq!(resp.status, PaymentStatus::Pending);

    let readiness = hub
        .get::<DependencyChecker>()
        .expect("DependencyChecker published");
    assert!(
        readiness.is_ready(),
        "a pinned dependency is resolved without a probe: {:?}",
        readiness.unresolved_deps()
    );

    cancel.cancel();
    drop(tokio::time::timeout(Duration::from_secs(5), run).await);
}
