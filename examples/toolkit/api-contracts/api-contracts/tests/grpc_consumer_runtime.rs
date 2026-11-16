//! A gear consuming `PaymentApi` over gRPC through the full `HostRuntime`
//! phases: the provider serves the contract through a gRPC hub that publishes
//! it in the directory, and `#[toolkit::consumes(.., transport = grpc)]` wires
//! a client that finds it there. The dependency gates readiness until the
//! service is published.
//!
//! A test binary of its own: `inventory` collects every `#[consumes]` linked
//! into the binary, and this one must be the only consumer here.

#![cfg(feature = "grpc-client")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::sync::Arc;
use std::time::{Duration, Instant};

use api_contracts_sdk::PAYMENT_API_GRPC_SERVICE;
use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::models::{ChargeRequest, PaymentStatus};
use async_trait::async_trait;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use toolkit::config::ConfigProvider;
use toolkit::contracts::{
    Gear, GrpcHubCapability, GrpcServiceCapability, RegisterGrpcServiceFn, RunnableCapability,
    SystemCapability,
};
use toolkit::registry::RegistryBuilder;
use toolkit::runtime::{Endpoint, GearInstance, GrpcInstallerStore, HostRuntime, SystemContext};
use toolkit::{ClientHub, DbOptions, DependencyChecker, DirectoryClient, GearCtx, GearManager};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use cf_api_contracts::api::grpc::PaymentApiGrpcService;
use cf_api_contracts::domain::service::PaymentDomainService;

const CONSUMER_GEAR: &str = "grpc-consumer";
const PROVIDER_GEAR: &str = "payment-provider";

/// The consumer under test. Its `owner_gear` is the kebab-case ident,
/// `grpc-consumer`, the name it is registered under below.
#[toolkit::consumes(
    contract = api_contracts_sdk::PaymentApi,
    from = "payment-provider",
    transport = grpc
)]
struct GrpcConsumer;

struct Plain;

#[async_trait]
impl Gear for Plain {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Serves `PaymentApi` over gRPC through the hub.
struct PaymentProvider {
    svc: Arc<PaymentDomainService>,
}

#[async_trait]
impl Gear for PaymentProvider {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl GrpcServiceCapability for PaymentProvider {
    async fn get_grpc_services(
        &self,
        _ctx: &GearCtx,
    ) -> anyhow::Result<Vec<RegisterGrpcServiceFn>> {
        let svc = Arc::clone(&self.svc);
        Ok(vec![RegisterGrpcServiceFn {
            service_name: PAYMENT_API_GRPC_SERVICE,
            register: Box::new(move |routes| {
                routes.add_service(PaymentApiGrpcService::new(Arc::clone(&svc)).into_server());
            }),
        }])
    }
}

/// A minimal gRPC hub: serves every collected service on an ephemeral port
/// and publishes each gear's services in the directory, as grpc-hub does.
struct MockHub {
    directory: Arc<GearManager>,
    installers: Mutex<Option<Arc<GrpcInstallerStore>>>,
    bound: Mutex<Option<String>>,
}

#[async_trait]
impl Gear for MockHub {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait]
impl SystemCapability for MockHub {
    fn wire_runtime(&self, sys: &SystemContext) -> anyhow::Result<()> {
        *self.installers.lock() = Some(Arc::clone(&sys.grpc_installers));
        Ok(())
    }
}

impl GrpcHubCapability for MockHub {
    fn bound_endpoint(&self) -> Option<String> {
        self.bound.lock().clone()
    }
}

#[async_trait]
impl RunnableCapability for MockHub {
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        let store = self.installers.lock().clone().expect("wire_runtime ran");
        let initial = store.take().expect("gRPC phase collected installers");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        *self.bound.lock() = Some(endpoint.clone());

        // System services come in the initial batch; application services,
        // like the provider's, in late batches once this hub runs. Collect
        // them all, then serve and publish them.
        let directory = Arc::clone(&self.directory);
        tokio::spawn(async move {
            let mut gears = initial.gears;
            if initial.more_batches {
                while let Some(mut batch) = store.next_late().await {
                    gears.append(&mut batch.gears);
                    batch.ack(Ok(()));
                }
            }
            let mut routes = tonic::service::RoutesBuilder::default();
            for gear in &gears {
                for installer in &gear.installers {
                    (installer.register)(&mut routes);
                }
            }
            let routes = routes.routes();
            let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
            let server = tokio::spawn(
                tonic::transport::Server::builder()
                    .add_routes(routes)
                    .serve_with_incoming_shutdown(incoming, cancel.cancelled_owned()),
            );
            for gear in &gears {
                let mut instance = GearInstance::new(&gear.gear_name, Uuid::new_v4());
                for installer in &gear.installers {
                    instance = instance.with_grpc_service(
                        installer.service_name,
                        Endpoint::from_uri(endpoint.clone()),
                    );
                }
                directory.register_instance(Arc::new(instance)).unwrap();
            }
            drop(server.await);
        });
        Ok(())
    }

    async fn stop(&self, _deadline: CancellationToken) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct EmptyConfig;
impl ConfigProvider for EmptyConfig {
    fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
        None
    }
}

#[tokio::test]
async fn a_grpc_consumer_finds_its_provider_through_the_runtime() {
    let _ = GrpcConsumer;
    let directory = Arc::new(GearManager::new());
    let hub_gear = Arc::new(MockHub {
        directory: Arc::clone(&directory),
        installers: Mutex::new(None),
        bound: Mutex::new(None),
    });
    let provider = Arc::new(PaymentProvider {
        svc: Arc::new(PaymentDomainService::new()),
    });

    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta(CONSUMER_GEAR, &[], Arc::new(Plain));
    builder.register_core_with_meta(PROVIDER_GEAR, &[], provider.clone() as Arc<dyn Gear>);
    builder
        .register_grpc_service_with_meta(PROVIDER_GEAR, provider as Arc<dyn GrpcServiceCapability>);
    builder.register_core_with_meta("grpc-hub", &[], hub_gear.clone() as Arc<dyn Gear>);
    builder.register_system_with_meta("grpc-hub", hub_gear.clone() as Arc<dyn SystemCapability>);
    builder.register_grpc_hub_with_meta("grpc-hub", hub_gear.clone() as Arc<dyn GrpcHubCapability>);
    builder.register_stateful_with_meta("grpc-hub", hub_gear as Arc<dyn RunnableCapability>);
    let registry = builder.build_topo_sorted().expect("registry build");

    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn DirectoryClient>(Arc::new(toolkit::LocalDirectoryClient::new(directory)));
    let cancel = CancellationToken::new();
    let runtime = HostRuntime::new(
        registry,
        Arc::new(EmptyConfig),
        DbOptions::None,
        Arc::clone(&hub),
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );
    let run = tokio::spawn(async move { runtime.run_gear_phases().await });

    let deadline = Instant::now() + Duration::from_secs(15);
    let consumer_view = hub.for_gear(CONSUMER_GEAR);
    let resp = loop {
        assert!(
            Instant::now() < deadline,
            "the gRPC consumer never reached its provider"
        );
        if let Ok(client) = consumer_view.get::<dyn PaymentApi>()
            && let Ok(resp) = client
                .charge(
                    SecurityContext::anonymous(),
                    ChargeRequest::new(500, "USD", "grpc e2e"),
                )
                .await
        {
            break resp;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(resp.status, PaymentStatus::Pending);
    assert!(
        hub.try_get::<dyn PaymentApi>().is_none(),
        "the proxy belongs to the consumer gear, not the process"
    );

    let readiness = hub
        .get::<DependencyChecker>()
        .expect("DependencyChecker published");
    let ready_deadline = Instant::now() + Duration::from_secs(15);
    while !readiness.is_ready() {
        assert!(
            Instant::now() < ready_deadline,
            "readiness stayed unresolved: {:?}",
            readiness.unresolved_deps()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    cancel.cancel();
    drop(tokio::time::timeout(Duration::from_secs(5), run).await);
}
