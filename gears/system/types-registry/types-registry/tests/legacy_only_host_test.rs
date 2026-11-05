#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! `mode: legacy_only` in a host whose new-API consumers use a remote registry: with a
//! database bound, the embedded registry still provides only the legacy client and v1
//! routes. Neither new contract is in the hub locally — the consumer gets resolving
//! clients — and the host's directory registration does not advertise the registry, so
//! discovery cannot route a new-API call back to it.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::api::{OpenApiRegistry, OpenApiRegistryImpl};
use toolkit::client_hub::ClientHub;
use toolkit::config::ConfigProvider;
use toolkit::contracts::{
    ApiGatewayCapability, DatabaseCapability, Gear, RestApiCapability, RunnableCapability,
    SystemCapability,
};
use toolkit::lifecycle::Status;
use toolkit::registry::RegistryBuilder;
use toolkit::runtime::{DbOptions, HostRuntime};
use toolkit::{
    DirectoryClient, GearCtx, RegisterInstanceInfo, RestHealthcheckRegistry, ServiceEndpoint,
    ServiceInstanceInfo,
};
use toolkit_gts::gts_id;
use types_registry::gear::TypesRegistryGear;
use types_registry_sdk::{PlatformTypesRegistryApi, TypesRegistryApi, TypesRegistryClient};

mod common;

const REGISTRY: &str = "types-registry";
const CONSUMER: &str = "consumes-fixture";
const GATEWAY: &str = "gateway-fixture";
const PROBE: &str = "probe-rest";
const CF_SEEDED: &str = gts_id!("cf.core.example.legacy_only_seeded.v1~");

/// Consumes both new contracts and does nothing else.
#[toolkit::consumes(contract = types_registry_sdk::PlatformTypesRegistryApi, from = "types-registry")]
#[toolkit::consumes(contract = types_registry_sdk::TypesRegistryApi, from = "types-registry")]
struct ConsumesFixture;

#[async_trait]
impl Gear for ConsumesFixture {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

/// A REST gear the directory phase does advertise: the positive control.
struct ProbeRest;

#[async_trait]
impl Gear for ProbeRest {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl RestApiCapability for ProbeRest {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }
}

/// A REST host that "binds" at once and keeps the composed operations.
#[derive(Default)]
struct GatewayFixture {
    openapi: OpenApiRegistryImpl,
}

#[async_trait]
impl Gear for GatewayFixture {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl ApiGatewayCapability for GatewayFixture {
    fn rest_prepare(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        _hc_registry: Arc<RestHealthcheckRegistry>,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }

    fn rest_finalize(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        _hc_registry: Arc<RestHealthcheckRegistry>,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }

    fn as_registry(&self) -> &dyn OpenApiRegistry {
        &self.openapi
    }

    fn bound_endpoint(&self) -> Option<String> {
        Some("http://127.0.0.1:9".to_owned())
    }
}

/// Records REST providers registered; knows no provider of anything.
#[derive(Default)]
struct RecordingDirectory {
    registered: Mutex<Vec<String>>,
}

#[async_trait]
impl DirectoryClient for RecordingDirectory {
    async fn resolve_grpc_service(&self, service: &str) -> anyhow::Result<ServiceEndpoint> {
        anyhow::bail!("no provider of {service}")
    }
    async fn resolve_rest_service(&self, gear: &str) -> anyhow::Result<ServiceEndpoint> {
        anyhow::bail!("no provider of {gear}")
    }
    async fn get_openapi_spec(&self, gear: &str) -> anyhow::Result<String> {
        anyhow::bail!("no provider of {gear}")
    }
    async fn list_instances(&self, _gear: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn list_all_instances(&self) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn register_instance(&self, info: RegisterInstanceInfo) -> anyhow::Result<()> {
        self.registered.lock().push(info.gear);
        Ok(())
    }
    async fn deregister_instance(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn send_heartbeat(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Config(HashMap<String, Value>);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
        self.0.get(gear_name)
    }
}

#[tokio::test]
async fn a_legacy_only_registry_serves_only_legacy_and_is_not_discovered() {
    let dir = common::TestDir::new("tr-legacy-only");
    let dsn = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("legacy-only.db").display()
    );
    let registry_entry = json!({
        "database": { "dsn": dsn, "pool": { "max_conns": 4 } },
        "config": {
            "mode": "legacy_only",
            "entities": [{
                "$id": format!("gts://{CF_SEEDED}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
            }],
        },
    });
    let figment = figment::Figment::new().merge(figment::providers::Serialized::defaults(
        json!({ "gears": { REGISTRY: registry_entry.clone() } }),
    ));
    let db_manager = Arc::new(
        toolkit_db::DbManager::from_figment(figment, dir.path().to_path_buf())
            .expect("database manager"),
    );

    let mut builder = RegistryBuilder::default();
    let gateway = Arc::new(GatewayFixture::default());
    builder.register_core_with_meta(GATEWAY, &[], gateway.clone() as Arc<dyn Gear>);
    builder.register_rest_host_with_meta(GATEWAY, gateway.clone() as Arc<dyn ApiGatewayCapability>);
    let lifecycle = Arc::new(TypesRegistryGear::default().into_gear());
    let gear = lifecycle.inner_arc();
    builder.register_core_with_meta(REGISTRY, &[], gear.clone() as Arc<dyn Gear>);
    builder.register_db_with_meta(REGISTRY, gear.clone() as Arc<dyn DatabaseCapability>);
    builder.register_system_with_meta(REGISTRY, gear.clone() as Arc<dyn SystemCapability>);
    builder.register_rest_with_meta(REGISTRY, gear as Arc<dyn RestApiCapability>);
    builder.register_stateful_with_meta(
        REGISTRY,
        Arc::clone(&lifecycle) as Arc<dyn RunnableCapability>,
    );
    builder.register_core_with_meta(
        CONSUMER,
        &[REGISTRY],
        Arc::new(ConsumesFixture) as Arc<dyn Gear>,
    );
    let probe = Arc::new(ProbeRest);
    builder.register_core_with_meta(PROBE, &[], probe.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta(PROBE, probe as Arc<dyn RestApiCapability>);
    let registry = builder.build_topo_sorted().expect("registry");

    let hub = Arc::new(ClientHub::default());
    let directory = Arc::new(RecordingDirectory::default());
    hub.register::<dyn DirectoryClient>(Arc::clone(&directory) as Arc<dyn DirectoryClient>);
    let cancel = CancellationToken::new();
    let config = Config(HashMap::from([
        (REGISTRY.to_owned(), registry_entry),
        (CONSUMER.to_owned(), json!({ "config": {} })),
    ]));
    let runtime = HostRuntime::new(
        registry,
        Arc::new(config),
        DbOptions::Manager(db_manager),
        Arc::clone(&hub),
        cancel.clone(),
        uuid::Uuid::new_v4(),
        None,
    );
    let mut host = tokio::spawn(runtime.run_gear_phases());

    // The directory phase runs after start: once the probe is advertised, it is done.
    tokio::select! {
        result = &mut host => panic!("host exited before advertising the probe: {result:?}"),
        () = common::await_delivery("the directory phase advertises the probe", || async {
            (lifecycle.status() == Status::Running
                && directory.registered.lock().iter().any(|g| g == PROBE))
            .then_some(())
        }) => {}
    }

    let registered_seen = { directory.registered.lock().clone() };
    assert_eq!(
        registered_seen,
        vec![PROBE.to_owned()],
        "the registry is not advertised as a REST provider"
    );
    // Neither new contract locally, database bound or not: the consumer resolves remotely.
    assert!(
        hub.try_get_local::<dyn PlatformTypesRegistryApi>()
            .is_none()
    );
    assert!(hub.try_get_local::<dyn TypesRegistryApi>().is_none());
    let consumer_hub = hub.for_gear(CONSUMER);
    assert!(
        consumer_hub.has_remote_proxy::<dyn PlatformTypesRegistryApi>()
            && consumer_hub.has_remote_proxy::<dyn TypesRegistryApi>(),
        "the consumer's resolving clients are wired"
    );
    // The legacy client keeps serving the legacy catalogue.
    let legacy = hub
        .get::<dyn TypesRegistryClient>()
        .expect("the legacy client");
    legacy
        .get_type_schema(CF_SEEDED)
        .await
        .expect("the configured entity is in the legacy catalogue");
    // Only v1 routes are composed.
    let paths: Vec<String> = gateway
        .openapi
        .operation_specs
        .iter()
        .map(|spec| spec.value().path.clone())
        .collect();
    assert!(
        paths.iter().any(|p| p.starts_with("/types-registry/v1/")),
        "{paths:?}"
    );
    assert!(
        paths.iter().all(|p| p.starts_with("/types-registry/v1/")),
        "no new-API route: {paths:?}"
    );

    cancel.cancel();
    host.await.expect("host task").expect("a clean shutdown");
}

#[test]
fn an_unknown_mode_fails_configuration() {
    let error = serde_json::from_value::<types_registry::config::TypesRegistryConfig>(
        json!({ "mode": "remote" }),
    )
    .expect_err("unknown mode");
    assert!(error.to_string().contains("unknown variant"), "{error}");

    let full = serde_json::from_value::<types_registry::config::TypesRegistryConfig>(json!({}))
        .expect("default");
    assert_eq!(full.mode, types_registry::config::RegistryMode::Full);
}
