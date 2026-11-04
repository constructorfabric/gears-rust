#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! `#[toolkit::consumes]` in a host that links the registry (SPEC §8.4, T35): the
//! registry's `provides` wiring publishes both contracts locally past the seed barrier,
//! and the proxy-wiring phase keeps them — local wins, and no resolving proxy is built.
//!
//! The host without the registry is `types-registry-sdk/tests/consumes_remote_test.rs`,
//! a separate binary: `inventory` is process-global, so one binary cannot be both.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::GearCtx;
use toolkit::client_hub::ClientHub;
use toolkit::config::ConfigProvider;
use toolkit::contracts::{DatabaseCapability, Gear, RunnableCapability, SystemCapability};
use toolkit::lifecycle::Status;
use toolkit::registry::RegistryBuilder;
use toolkit::runtime::{DbOptions, HostRuntime};
use toolkit_gts::gts_id;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry::gear::TypesRegistryGear;
use types_registry_sdk::{
    BatchGetEntitiesRequest, EntityKey, EntityLookup, PlatformTypesRegistryApi, Projection,
    TypesRegistryApi,
};

mod common;

const REGISTRY: &str = "types-registry";
/// The kebab case of [`ConsumesFixture`], the name `#[toolkit::consumes]` derives.
const CONSUMER: &str = "consumes-fixture";
const CF_SEEDED: &str = gts_id!("cf.core.example.seeded.v1~");

/// Consumes both contracts and does nothing else.
#[toolkit::consumes(contract = types_registry_sdk::PlatformTypesRegistryApi, from = "types-registry")]
#[toolkit::consumes(contract = types_registry_sdk::TypesRegistryApi, from = "types-registry")]
struct ConsumesFixture;

#[async_trait]
impl Gear for ConsumesFixture {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Config(HashMap<String, Value>);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
        self.0.get(gear_name)
    }
}

fn read() -> BatchGetEntitiesRequest {
    BatchGetEntitiesRequest {
        items: vec![EntityKey::GtsId(gts::GtsId::try_new(CF_SEEDED).unwrap()).into()],
        projection: Projection::Default,
        fresh: false,
    }
}

#[tokio::test]
async fn a_host_that_links_the_registry_keeps_both_local_apis() {
    let dir = common::TestDir::new("tr-consumes-local");
    let dsn = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("consumes.db").display()
    );
    let registry_entry = json!({
        "database": { "dsn": dsn, "pool": { "max_conns": 4 } },
        "config": { "entities": [{
            "$id": format!("gts://{CF_SEEDED}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
        }] },
    });
    let figment = figment::Figment::new().merge(figment::providers::Serialized::defaults(
        json!({ "gears": { REGISTRY: registry_entry.clone() } }),
    ));
    let db_manager = Arc::new(
        toolkit_db::DbManager::from_figment(figment, dir.path().to_path_buf())
            .expect("database manager"),
    );

    let mut builder = RegistryBuilder::default();
    let lifecycle = Arc::new(TypesRegistryGear::default().into_gear());
    let gear = lifecycle.inner_arc();
    builder.register_core_with_meta(REGISTRY, &[], gear.clone() as Arc<dyn Gear>);
    builder.register_db_with_meta(REGISTRY, gear.clone() as Arc<dyn DatabaseCapability>);
    builder.register_system_with_meta(REGISTRY, gear as Arc<dyn SystemCapability>);
    builder.register_stateful_with_meta(
        REGISTRY,
        Arc::clone(&lifecycle) as Arc<dyn RunnableCapability>,
    );
    builder.register_core_with_meta(
        CONSUMER,
        &[REGISTRY],
        Arc::new(ConsumesFixture) as Arc<dyn Gear>,
    );
    let registry = builder.build_topo_sorted().expect("registry");

    let hub = Arc::new(ClientHub::default());
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
    let host = tokio::spawn(runtime.run_gear_phases());

    // Running means every phase before it is done, proxy wiring included.
    tokio::time::timeout(
        Duration::from_secs(60),
        common::await_delivery("the registry runnable starts", || async {
            (lifecycle.status() == Status::Running).then_some(())
        }),
    )
    .await
    .expect("the host boots");

    assert!(
        hub.try_get_local::<dyn PlatformTypesRegistryApi>()
            .is_some()
    );
    assert!(hub.try_get_local::<dyn TypesRegistryApi>().is_some());
    assert!(
        !hub.has_remote_proxy::<dyn PlatformTypesRegistryApi>()
            && !hub.has_remote_proxy::<dyn TypesRegistryApi>(),
        "local wins: no resolving proxy is wired beside the registry"
    );

    let platform = hub
        .get::<dyn PlatformTypesRegistryApi>()
        .expect("the platform API");
    let tenant = hub.get::<dyn TypesRegistryApi>().expect("the tenant API");
    for found in [
        platform
            .batch_get_entities(&PlatformSecurityContext::outbound_marker(), read())
            .await
            .expect("read"),
        tenant
            .batch_get_entities(&SecurityContext::anonymous(), read())
            .await
            .expect("read"),
    ] {
        assert!(
            matches!(found.0.values().next(), Some(EntityLookup::Found { .. })),
            "the seeded database answers: {found:?}"
        );
    }

    cancel.cancel();
    host.await.expect("host task").expect("a clean shutdown");
}
