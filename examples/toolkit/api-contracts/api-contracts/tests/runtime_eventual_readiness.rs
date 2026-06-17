//! Runtime end-to-end test for eventual readiness, driven through the real
//! `HostRuntime` lifecycle phases.
//!
//! A consumer declares `#[toolkit::consumes(contract = PaymentApi, from = ...)]`.
//! A provider gear serves the `PaymentApi` REST routes (but registers NO local
//! impl in the shared `ClientHub`, so the consumer must go over REST). A minimal
//! mock gateway host binds a real TCP listener and publishes its bound endpoint.
//!
//! Running `HostRuntime::run_gear_phases` exercises both Phase-2 phases end to
//! end:
//! - **proxy-wiring** (after init): replays the macro-generated
//!   `ConsumerRegistration`, registering the directory-resolving `PaymentApi`
//!   client into the hub (no local impl → resolving client wins).
//! - **directory-register** (after start): advertises the provider's REST
//!   endpoint in the directory once the gateway has bound.
//!
//! The consumer then resolves the provider over HTTP and a `charge` call
//! succeeds — proving the full eventual-readiness path through the runtime.

#![allow(clippy::unwrap_used)]
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
use toolkit::{
    ClientHub, DbOptions, DirectoryClient, GearCtx, GearManager, LocalDirectoryClient,
    ReadinessPhase, ReadinessState,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::policy::PolicyStack;
use toolkit_security::SecurityContext;

use cf_api_contracts::api::rest::routes::register_routes;
use cf_api_contracts::client::local::PaymentLocalClient;
use cf_api_contracts::domain::service::PaymentDomainService;

const PROVIDER_GEAR: &str = "payment-provider";

// ----- Consumer: declares the dependency via the macro under test -----------

/// The macro emits a `ConsumerRegistration` (behind `directory-rest-client`)
/// that the proxy-wiring phase replays. The struct itself is inert.
#[toolkit::consumes(contract = api_contracts_sdk::PaymentApi, from = "payment-provider")]
struct PaymentConsumer;

// ----- Provider: serves REST routes, registers NO local impl in the hub ------

struct PaymentProvider {
    svc: Arc<PaymentDomainService>,
}

#[async_trait]
impl Gear for PaymentProvider {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        // Intentionally does NOT register a local `PaymentApi` in the ClientHub,
        // so the in-process consumer is forced through the REST resolving path.
        Ok(())
    }
}

impl RestApiCapability for PaymentProvider {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        let service: Arc<dyn PaymentApi> = Arc::new(PaymentLocalClient::new(
            Arc::clone(&self.svc),
            Arc::new(PolicyStack::new()),
        ));
        Ok(register_routes(router, openapi, service))
    }
}

// ----- Minimal mock REST host (ApiGatewayCap + RunnableCap) ------------------

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
    fn rest_prepare(&self, _ctx: &GearCtx, router: Router) -> anyhow::Result<Router> {
        Ok(router)
    }

    fn rest_finalize(&self, _ctx: &GearCtx, router: Router) -> anyhow::Result<Router> {
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

        // Inject an anonymous SecurityContext per request, as a real gateway
        // would after authn — the PaymentApi handlers read it from extensions.
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
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(shutdown)
                .await;
        });
        Ok(())
    }

    async fn stop(&self, _deadline: CancellationToken) -> anyhow::Result<()> {
        Ok(())
    }
}

// ----- Config provider (empty) ----------------------------------------------

#[derive(Default)]
struct EmptyConfig;
impl ConfigProvider for EmptyConfig {
    fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
        None
    }
}

fn charge_req() -> ChargeRequest {
    ChargeRequest::new(1000, "USD", "runtime eventual-readiness e2e")
}

#[tokio::test]
async fn consumer_resolves_provider_through_runtime_phases() {
    // Keep the consumer registration linked into this test binary's inventory.
    let _ = PaymentConsumer;

    // Build a registry: provider (REST) + mock gateway host (rest_host + runnable).
    let provider = Arc::new(PaymentProvider {
        svc: Arc::new(PaymentDomainService::new()),
    });
    let gateway = Arc::new(MockGateway::new());

    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta(PROVIDER_GEAR, &[], provider.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta(PROVIDER_GEAR, provider as Arc<dyn RestApiCapability>);
    builder.register_core_with_meta("mock-gateway", &[], gateway.clone() as Arc<dyn Gear>);
    builder
        .register_rest_host_with_meta("mock-gateway", gateway.clone() as Arc<dyn ApiGatewayCapability>);
    builder.register_stateful_with_meta("mock-gateway", gateway as Arc<dyn RunnableCapability>);
    let registry = builder.build_topo_sorted().expect("registry build");

    // Directory backed by a GearManager, wired into the hub before phases run.
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

    // run_gear_phases blocks on cancellation after start; drive it in the bg.
    let run = tokio::spawn(async move { runtime.run_gear_phases().await });

    // The proxy-wiring phase registers the resolving PaymentApi client into the
    // hub; the directory-register phase (after the gateway binds) advertises the
    // provider. Poll a charge call until eventual readiness converges.
    let deadline = Instant::now() + Duration::from_secs(15);
    let resp = loop {
        assert!(Instant::now() < deadline, "eventual readiness did not converge");
        if let Ok(client) = hub.get::<dyn PaymentApi>() {
            match client.charge(SecurityContext::anonymous(), charge_req()).await {
                Ok(resp) => break resp,
                Err(CanonicalError::ServiceUnavailable { .. }) => { /* not ready yet */ }
                Err(other) => panic!("unexpected error: {other:?}"),
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert_eq!(resp.status, PaymentStatus::Pending);
    assert!(!resp.payment_id.is_nil());

    // Process readiness reflects eventual readiness: the readiness probe loop
    // (proxy-wiring phase) flips the consumed provider to resolved once it is
    // advertised in the directory, so ReadinessState goes Starting -> Ready.
    let readiness = hub
        .get::<ReadinessState>()
        .expect("ReadinessState published in ClientHub by the runtime");
    let rdy_deadline = Instant::now() + Duration::from_secs(15);
    while !readiness.is_ready() {
        assert!(
            Instant::now() < rdy_deadline,
            "readiness did not reach Ready; unresolved: {:?}",
            readiness.report().unresolved_deps
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(readiness.report().phase, ReadinessPhase::Ready);

    // Shutdown → the draining watcher flips readiness to Draining (so /readyz 503).
    cancel.cancel();
    let drain_deadline = Instant::now() + Duration::from_secs(5);
    while readiness.report().phase != ReadinessPhase::Draining {
        assert!(Instant::now() < drain_deadline, "readiness did not reach Draining");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let _ = tokio::time::timeout(Duration::from_secs(5), run).await;
}
