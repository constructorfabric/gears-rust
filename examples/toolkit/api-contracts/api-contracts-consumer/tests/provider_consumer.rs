//! Two-crate provider + consumer composition, booted together through the real
//! `HostRuntime` (embedded / Profile-1, local-wins).
//!
//! - Provider: the real `cf_api_contracts::ApiContracts` gear. Its
//!   `#[toolkit::provides]` wiring defaults to `ClientWiring::Local`, so in
//!   `init` it registers a local `Arc<dyn PaymentApi>` in the shared `ClientHub`.
//! - Consumer: this crate's `ApiContractsConsumer` gear. Its
//!   `#[toolkit::consumes]` `ConsumerRegistration` is replayed by the
//!   proxy-wiring phase; it sees the provider's local impl already present and
//!   short-circuits (local-wins) — no REST client is wired.
//! - The consumer then fulfils work via `ChargeProxyService`, which resolves
//!   `dyn PaymentApi` from the hub and forwards the charge — entirely in-process.
//!
//! The remote / directory-resolving path (no local impl → REST resolving client)
//! is covered by `api-contracts/tests/runtime_eventual_readiness.rs`; here we
//! prove the embedded two-gear wiring.

#![allow(clippy::unwrap_used)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::Router;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::models::{ChargeRequest, PaymentStatus};

use toolkit::api::{OpenApiRegistry, OpenApiRegistryImpl};
use toolkit::config::ConfigProvider;
use toolkit::contracts::{ApiGatewayCapability, Gear, RestApiCapability, RunnableCapability};
use toolkit::registry::RegistryBuilder;
use toolkit::runtime::HostRuntime;
use toolkit::{ClientHub, DbOptions, DirectoryClient, GearCtx, GearManager, LocalDirectoryClient};
use toolkit_security::SecurityContext;

use cf_api_contracts::ApiContracts;
use cf_api_contracts::api::rest::routes::register_routes;
use cf_api_contracts::domain::local_client::PaymentLocalClient;
use cf_api_contracts::domain::service::PaymentDomainService;
use cf_api_contracts_consumer::{ApiContractsConsumer, ChargeProxyService};
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::policy::PolicyStack;

const PROVIDER_GEAR: &str = "api-contracts";

// ----- Minimal mock REST host (ApiGatewayCap + RunnableCap) ------------------
// Adapted from `api-contracts/tests/runtime_eventual_readiness.rs`: the REST +
// directory-register phases of `run_gear_phases` expect a gateway host.

struct MockGateway {
    openapi: OpenApiRegistryImpl,
    final_router: Mutex<Option<Router>>,
    bound: Mutex<Option<String>>,
}

impl MockGateway {
    fn new() -> Self {
        Self {
            openapi: OpenApiRegistryImpl::new(),
            final_router: Mutex::new(None),
            bound: Mutex::new(None),
        }
    }
}

#[async_trait]
impl Gear for MockGateway {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl ApiGatewayCapability for MockGateway {
    fn rest_prepare(
        &self,
        _ctx: &GearCtx,
        router: Router,
        _hc_registry: std::sync::Arc<toolkit::RestHealthcheckRegistry>,
    ) -> anyhow::Result<Router> {
        Ok(router)
    }

    fn rest_finalize(
        &self,
        _ctx: &GearCtx,
        router: Router,
        _hc_registry: std::sync::Arc<toolkit::RestHealthcheckRegistry>,
    ) -> anyhow::Result<Router> {
        *self.final_router.lock() = Some(router.clone());
        Ok(router)
    }

    fn as_registry(&self) -> &dyn OpenApiRegistry {
        &self.openapi
    }

    fn bound_endpoint(&self) -> Option<String> {
        self.bound.lock().clone()
    }
}

#[async_trait]
impl RunnableCapability for MockGateway {
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        let router = self
            .final_router
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("router not finalized before start"))?;

        let router = router.layer(axum::middleware::from_fn(
            |mut req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| async move {
                req.extensions_mut().insert(SecurityContext::anonymous());
                next.run(req).await
            },
        ));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        *self.bound.lock() = Some(format!("http://{addr}"));

        let shutdown = async move { cancel.cancelled().await };
        tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown)
                .await
                .ok();
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

fn charge_req() -> ChargeRequest {
    ChargeRequest::new(1000, "USD", "provider+consumer embedded e2e")
}

#[tokio::test]
async fn consumer_resolves_provider_local_impl_through_the_hub() {
    // Keep the consumer's `#[toolkit::consumes]` ConsumerRegistration linked into
    // this test binary's inventory.
    let _ = ApiContractsConsumer;

    // Registry: real provider gear + this consumer gear + a mock REST host.
    let provider = Arc::new(ApiContracts);
    let consumer = Arc::new(ApiContractsConsumer);
    let gateway = Arc::new(MockGateway::new());

    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta(PROVIDER_GEAR, &[], provider.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta(PROVIDER_GEAR, provider as Arc<dyn RestApiCapability>);
    builder.register_core_with_meta(
        "api-contracts-consumer",
        &[],
        consumer.clone() as Arc<dyn Gear>,
    );
    builder.register_rest_with_meta(
        "api-contracts-consumer",
        consumer as Arc<dyn RestApiCapability>,
    );
    builder.register_core_with_meta("mock-gateway", &[], gateway.clone() as Arc<dyn Gear>);
    builder.register_rest_host_with_meta(
        "mock-gateway",
        gateway.clone() as Arc<dyn ApiGatewayCapability>,
    );
    builder.register_stateful_with_meta("mock-gateway", gateway as Arc<dyn RunnableCapability>);
    let registry = builder.build_topo_sorted().expect("registry build");

    // A directory must be in the hub for the proxy-wiring phase to replay the
    // consumer registration (which then short-circuits on the local impl).
    let gear_mgr = Arc::new(GearManager::new());
    let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(gear_mgr));
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn DirectoryClient>(dir);

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

    // The provider's init registers a local `PaymentApi` in the hub; wait for it.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "provider never registered PaymentApi"
        );
        if hub.get::<dyn PaymentApi>().is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The consumer's own code (ChargeProxyService) resolves `PaymentApi` from the
    // shared hub and forwards a charge — in-process, local-wins.
    let proxy = ChargeProxyService::new(Arc::clone(&hub));
    let resp = proxy
        .place_charge(SecurityContext::anonymous(), charge_req())
        .await
        .expect("consumer should resolve the provider's local PaymentApi and charge");

    assert_eq!(resp.status, PaymentStatus::Pending);
    assert!(!resp.payment_id.is_nil());

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), run).await.ok();
}

// ----- OoP path: the consumer uses the *generated* REST client ---------------

/// A provider that serves the SDK's **generated** `PaymentApi` REST routes
/// (`register_payment_api_rest_routes`, via `cf_api_contracts`'s `register_routes`)
/// but registers **no** local `PaymentApi` in the hub — forcing the consumer
/// through the auto-generated `PaymentApiRestResolvingClient`.
struct RestOnlyProvider {
    svc: Arc<PaymentDomainService>,
}

#[async_trait]
impl Gear for RestOnlyProvider {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl RestApiCapability for RestOnlyProvider {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        // Backs the generated server routes with the real domain impl, but does
        // NOT put a `PaymentApi` into the ClientHub.
        let service: Arc<dyn PaymentApi> = Arc::new(PaymentLocalClient::new(
            Arc::clone(&self.svc),
            Arc::new(PolicyStack::new()),
        ));
        Ok(register_routes(router, openapi, service))
    }
}

#[tokio::test]
async fn consumer_resolves_provider_via_generated_rest_client() {
    // Keep the consumer's ConsumerRegistration linked into the binary.
    let _ = ApiContractsConsumer;

    let provider = Arc::new(RestOnlyProvider {
        svc: Arc::new(PaymentDomainService::new()),
    });
    let consumer = Arc::new(ApiContractsConsumer);
    let gateway = Arc::new(MockGateway::new());

    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta(PROVIDER_GEAR, &[], provider.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta(PROVIDER_GEAR, provider as Arc<dyn RestApiCapability>);
    builder.register_core_with_meta(
        "api-contracts-consumer",
        &[],
        consumer.clone() as Arc<dyn Gear>,
    );
    builder.register_rest_with_meta(
        "api-contracts-consumer",
        consumer as Arc<dyn RestApiCapability>,
    );
    builder.register_core_with_meta("mock-gateway", &[], gateway.clone() as Arc<dyn Gear>);
    builder.register_rest_host_with_meta(
        "mock-gateway",
        gateway.clone() as Arc<dyn ApiGatewayCapability>,
    );
    builder.register_stateful_with_meta("mock-gateway", gateway as Arc<dyn RunnableCapability>);
    let registry = builder.build_topo_sorted().expect("registry build");

    let gear_mgr = Arc::new(GearManager::new());
    let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(gear_mgr));
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn DirectoryClient>(dir);

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

    // No local `PaymentApi` exists, so proxy-wiring registers the generated
    // `PaymentApiRestResolvingClient`. directory-register (after the gateway
    // binds) advertises the provider; the resolving client then reaches it over
    // real HTTP. Poll the consumer's charge until eventual readiness converges.
    let proxy = ChargeProxyService::new(Arc::clone(&hub));
    let deadline = Instant::now() + Duration::from_secs(15);
    let resp = loop {
        assert!(
            Instant::now() < deadline,
            "eventual readiness did not converge"
        );
        match proxy
            .place_charge(SecurityContext::anonymous(), charge_req())
            .await
        {
            Ok(resp) => break resp,
            Err(CanonicalError::ServiceUnavailable { .. }) => { /* not ready yet */ }
            Err(other) => panic!("unexpected error: {other:?}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // The charge round-tripped: consumer → generated resolving REST client →
    // HTTP → gateway → provider's generated routes → domain impl.
    assert_eq!(resp.status, PaymentStatus::Pending);
    assert!(!resp.payment_id.is_nil());

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), run).await.ok();
}
