//! Staged REST orchestration through the runtime, without binding a socket.

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

use super::{DbOptions, HostRuntime};
use crate::api::OpenApiRegistryImpl;
use crate::client_hub::ClientHub;
use crate::config::ConfigProvider;
use crate::contracts::{
    ApiGatewayCapability, Gear, OpenApiRegistry, RestApiCapability, RunnableCapability,
    SystemCapability,
};
use crate::gts_publication::{
    DeclarationSource, GtsPublicationBackend, GtsPublicationCapability, GtsPublicationDescriptor,
};
use crate::healthcheck::RestHealthcheckRegistry;
use crate::registry::RegistryError;
use crate::{DirectoryClient, GearCtx, RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo};

#[derive(Default)]
struct State {
    events: Mutex<Vec<String>>,
    bootstrap: Mutex<Option<Router>>,
    final_router: Mutex<Option<Router>>,
    publishing: Notify,
    admitted: Notify,
    started: Notify,
    refuse: bool,
    /// The staged host call that fails: `bootstrap`, `system` or `final`.
    fail: Option<&'static str>,
}

impl State {
    fn record(&self, event: impl Into<String>) {
        self.events.lock().push(event.into());
    }

    fn fail_at(&self, stage: &str) -> anyhow::Result<()> {
        anyhow::ensure!(self.fail != Some(stage), "{stage} failed");
        Ok(())
    }
}

struct Gateway {
    state: Arc<State>,
    openapi: OpenApiRegistryImpl,
}

#[async_trait]
impl Gear for Gateway {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        self.state.record("init:gateway");
        Ok(())
    }
}

#[async_trait]
impl ApiGatewayCapability for Gateway {
    async fn rest_bootstrap(
        &self,
        ctx: &GearCtx,
        health: Arc<RestHealthcheckRegistry>,
        _cancel: CancellationToken,
    ) -> anyhow::Result<Option<Router>> {
        self.state.record("bind");
        self.state.fail_at("bootstrap")?;
        self.rest_prepare(ctx, Router::new(), health).map(Some)
    }

    fn rest_finalize_bootstrap(
        &self,
        _ctx: &GearCtx,
        router: Router,
        _health: Arc<RestHealthcheckRegistry>,
    ) -> anyhow::Result<()> {
        self.state.record("system-routes");
        self.state.fail_at("system")?;
        *self.state.bootstrap.lock() = Some(router);
        Ok(())
    }

    async fn rest_shutdown_bootstrap(&self) -> anyhow::Result<()> {
        self.state.record("bootstrap-stopped");
        Ok(())
    }

    fn rest_prepare(
        &self,
        _ctx: &GearCtx,
        router: Router,
        _health: Arc<RestHealthcheckRegistry>,
    ) -> anyhow::Result<Router> {
        self.state.record("prepare");
        Ok(router.route("/base", get(|| async { "base" })))
    }

    fn rest_finalize(
        &self,
        _ctx: &GearCtx,
        router: Router,
        _health: Arc<RestHealthcheckRegistry>,
    ) -> anyhow::Result<Router> {
        self.state.record("final-routes");
        self.state.fail_at("final")?;
        *self.state.final_router.lock() = Some(router.clone());
        Ok(router)
    }

    fn as_registry(&self) -> &dyn OpenApiRegistry {
        &self.openapi
    }

    fn bound_endpoint(&self) -> Option<String> {
        Some("http://test-host:8080".to_owned())
    }
}

#[async_trait]
impl RunnableCapability for Gateway {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        self.state.record("start:gateway");
        self.state.started.notify_one();
        Ok(())
    }
    async fn stop(&self, _deadline: CancellationToken) -> anyhow::Result<()> {
        self.state.record("stop:gateway");
        Ok(())
    }
}

struct Provider {
    name: &'static str,
    route: &'static str,
    state: Arc<State>,
}

#[async_trait]
impl Gear for Provider {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        self.state.record(format!("init:{}", self.name));
        Ok(())
    }
}

impl SystemCapability for Provider {}

impl RestApiCapability for Provider {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        self.state.record(format!("route:{}", self.name));
        Ok(router.route(self.route, get(|| async { "ok" })))
    }
}

fn backend(hub: &ClientHub) -> anyhow::Result<GtsPublicationBackend> {
    let state = hub.get::<State>()?;
    Ok(GtsPublicationBackend::new(move |_| {
        let state = Arc::clone(&state);
        async move {
            state.record("publication");
            state.publishing.notify_one();
            anyhow::ensure!(!state.refuse, "publication refused");
            state.admitted.notified().await;
            Ok(())
        }
    }))
}

static PUBLICATION: GtsPublicationDescriptor = GtsPublicationDescriptor {
    gear: "application",
    publisher_version: "1.0.0",
    declarations: || {
        vec![DeclarationSource::new(
            "test",
            vec![(
                "gts.cf.test.bootstrap.widget.v1~".to_owned(),
                "{}".to_owned(),
            )],
        )]
    },
    entities: None,
    backend,
    provider: "registry",
    wire: |_, _, _| Ok(crate::discovery::WireOutcome::Local),
};

impl GtsPublicationCapability for Provider {
    fn descriptor(&self) -> &'static GtsPublicationDescriptor {
        &PUBLICATION
    }
}

struct Directory(Arc<State>);

#[async_trait]
impl DirectoryClient for Directory {
    async fn register_instance(&self, info: RegisterInstanceInfo) -> anyhow::Result<()> {
        self.0.record(format!("advertise:{}", info.gear));
        Ok(())
    }
    async fn deregister_instance(&self, gear: &str, _instance: &str) -> anyhow::Result<()> {
        self.0.record(format!("deregister:{gear}"));
        Ok(())
    }
    async fn list_instances(&self, _gear: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(Vec::new())
    }
    async fn list_all_instances(&self) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(Vec::new())
    }
    async fn resolve_grpc_service(&self, _name: &str) -> anyhow::Result<ServiceEndpoint> {
        anyhow::bail!("unused")
    }
    async fn resolve_rest_service(&self, _name: &str) -> anyhow::Result<ServiceEndpoint> {
        anyhow::bail!("unused")
    }
    async fn get_openapi_spec(&self, _gear: &str) -> anyhow::Result<String> {
        Ok(String::new())
    }
    async fn send_heartbeat(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

struct NoConfig;
impl ConfigProvider for NoConfig {
    fn get_gear_config(&self, _name: &str) -> Option<&serde_json::Value> {
        None
    }
}

fn runtime(state: &Arc<State>, cancel: &CancellationToken) -> HostRuntime {
    let gateway = Arc::new(Gateway {
        state: Arc::clone(state),
        openapi: OpenApiRegistryImpl::new(),
    });
    let system = Arc::new(Provider {
        name: "registry",
        route: "/system",
        state: Arc::clone(state),
    });
    let app = Arc::new(Provider {
        name: "application",
        route: "/application",
        state: Arc::clone(state),
    });
    let mut builder = crate::registry::RegistryBuilder::default();
    builder.register_core_with_meta("gateway", &[], gateway.clone());
    builder.register_rest_host_with_meta("gateway", gateway.clone());
    builder.register_stateful_with_meta("gateway", gateway);
    builder.register_core_with_meta("registry", &[], system.clone());
    builder.register_system_with_meta("registry", system.clone());
    builder.register_rest_with_meta("registry", system);
    builder.register_core_with_meta("application", &[], app.clone());
    builder.register_rest_with_meta("application", app.clone());
    builder.register_gts_publication_with_meta("application", app);
    let hub = Arc::new(ClientHub::new());
    hub.register::<State>(Arc::clone(state));
    hub.register::<dyn DirectoryClient>(Arc::new(Directory(Arc::clone(state))));
    HostRuntime::new(
        builder.build_topo_sorted().unwrap(),
        Arc::new(NoConfig),
        DbOptions::None,
        hub,
        cancel.clone(),
        uuid::Uuid::new_v4(),
        None,
    )
}

async fn status(router: &Router, path: &str) -> u16 {
    router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test]
async fn embedded_system_routes_are_served_and_advertised_before_publication() {
    let state = Arc::new(State::default());
    let cancel = CancellationToken::new();
    let mut running = tokio::spawn(runtime(&state, &cancel).run_gear_phases());
    tokio::select! {
        () = state.publishing.notified() => {}
        result = &mut running => panic!("startup ended before publication: {result:?}"),
    }
    let pending_events = state.events.lock().clone();
    assert_eq!(
        pending_events,
        [
            "bind",
            "prepare",
            "init:registry",
            "route:registry",
            "system-routes",
            "advertise:registry",
            "publication"
        ]
    );
    let bootstrap = state
        .bootstrap
        .lock()
        .clone()
        .expect("system router is live");
    assert_eq!(status(&bootstrap, "/base").await, 200);
    assert_eq!(status(&bootstrap, "/system").await, 200);
    assert_eq!(status(&bootstrap, "/application").await, 404);

    state.admitted.notify_one();
    tokio::select! {
        () = state.started.notified() => {}
        result = &mut running => panic!("startup ended before the gateway started: {result:?}"),
    }
    let final_router = state
        .final_router
        .lock()
        .clone()
        .expect("full router is live");
    for path in ["/base", "/system", "/application"] {
        assert_eq!(status(&final_router, path).await, 200, "{path}");
    }
    cancel.cancel();
    running.await.unwrap().unwrap();
    let events = state.events.lock().clone();
    for event in [
        "prepare",
        "route:registry",
        "route:application",
        "final-routes",
        "advertise:registry",
    ] {
        assert_eq!(
            events
                .iter()
                .filter(|actual| actual.as_str() == event)
                .count(),
            1,
            "{event}: {events:?}"
        );
    }
}

#[tokio::test]
async fn refused_publication_joins_the_bootstrap_host_without_starting_applications() {
    let state = Arc::new(State {
        refuse: true,
        ..State::default()
    });
    let error = runtime(&state, &CancellationToken::new())
        .run_gear_phases()
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("publication refused"));
    let events = state.events.lock().clone();
    assert!(events.iter().any(|event| event == "bind"), "{events:?}");
    assert!(
        events.iter().any(|event| event == "bootstrap-stopped"),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .position(|e| e == "deregister:registry")
            .unwrap()
            < events
                .iter()
                .position(|e| e == "bootstrap-stopped")
                .unwrap(),
        "discovery must stop routing before the listener drains: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event == "start:gateway" || event == "init:application"),
        "{events:?}"
    );
}

/// Staged errors retain the same host/phase context as ordinary REST errors.
#[tokio::test]
async fn staged_rest_host_failures_carry_the_host_gear_and_phase() {
    for stage in ["bootstrap", "system", "final"] {
        let state = Arc::new(State {
            fail: Some(stage),
            ..State::default()
        });
        // Admission is granted up front, so the final stage is reached.
        state.admitted.notify_one();
        let error = runtime(&state, &CancellationToken::new())
            .run_gear_phases()
            .await
            .unwrap_err();
        let registry_error = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<RegistryError>())
            .unwrap_or_else(|| panic!("{stage}: no RegistryError in {error:#}"));
        let gear = match (stage, registry_error) {
            ("bootstrap", RegistryError::RestPrepare { gear, .. })
            | ("system" | "final", RegistryError::RestFinalize { gear, .. }) => *gear,
            _ => panic!("{stage}: unexpected {registry_error:?}"),
        };
        assert_eq!(gear, "gateway", "{stage}");
        assert!(
            format!("{error:#}").contains(&format!("{stage} failed")),
            "{stage}: the host error is the source: {error:#}"
        );
    }
}
