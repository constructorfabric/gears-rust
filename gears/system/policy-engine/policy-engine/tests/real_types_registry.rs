//! The gear against the real types-registry gear, in the platform's phase
//! order: every `init` runs while the registry is still in configuration
//! mode (type lookups answer "not found"), and the registry turns ready in
//! its `post_init`, before the policy engine's.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::gear_ctx_over;
use policy_engine::PolicyEngine;
use serde_json::json;
use toolkit::config::ConfigProvider;
use toolkit::contracts::{DatabaseCapability, SystemCapability};
use toolkit::runtime::{GearManager, GrpcInstallerStore, SystemContext};
use toolkit::{ClientHub, Gear, GearCtx};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use types_registry::gear::TypesRegistryGear;
use types_registry_sdk::TypesRegistryClient;
use uuid::Uuid;

/// A concrete type the registry knows from the platform's inventory.
const KNOWN_TYPE: &str = "gts.cf.toolkit.authz.permission.v1~";

struct NoConfig;

impl ConfigProvider for NoConfig {
    fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
        None
    }
}

fn system() -> SystemContext {
    SystemContext::new(
        Uuid::new_v4(),
        Arc::new(GearManager::new()),
        Arc::new(GrpcInstallerStore::new()),
    )
}

/// The real types-registry gear, initialised into `hub`.
async fn types_registry(hub: &Arc<ClientHub>) -> TypesRegistryGear {
    let gear = TypesRegistryGear::default();
    let db = connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    run_migrations_for_testing(&db, gear.migrations())
        .await
        .unwrap();
    let ctx = GearCtx::new(
        "types-registry",
        Uuid::new_v4(),
        Arc::new(NoConfig),
        Arc::clone(hub),
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db(DBProvider::new(db));
    gear.init(&ctx).await.expect("types-registry init");
    gear
}

fn builtin_over(resource_type: &str) -> serde_json::Value {
    json!({ "builtin_policies": [{
        "id": "guard",
        "resource_types": [resource_type],
        "content": "package p\ndeny := false",
    }] })
}

#[tokio::test]
async fn a_builtin_over_a_concrete_type_starts_and_is_checked_once_the_registry_is_ready() {
    let hub = Arc::new(ClientHub::new());
    let registry = types_registry(&hub).await;
    let client = hub.get::<dyn TypesRegistryClient>().unwrap();
    assert!(
        client.get_type_schema(KNOWN_TYPE).await.is_err(),
        "before its post_init the registry answers no type lookup"
    );
    let ctx = gear_ctx_over(&builtin_over(KNOWN_TYPE), Arc::clone(&hub)).await;
    let engine = PolicyEngine::default();
    engine
        .init(&ctx)
        .await
        .expect("init must not ask the registry before it is ready");

    registry.post_init(&system()).await.expect("registry ready");
    engine
        .post_init(&system())
        .await
        .expect("the concrete type is known");
}

#[tokio::test]
async fn a_builtin_over_an_unknown_concrete_type_fails_startup_in_post_init() {
    let hub = Arc::new(ClientHub::new());
    let registry = types_registry(&hub).await;
    let ctx = gear_ctx_over(
        &builtin_over("gts.cf.core.example.gadget.v1~"),
        Arc::clone(&hub),
    )
    .await;
    let engine = PolicyEngine::default();
    engine.init(&ctx).await.expect("init");

    registry.post_init(&system()).await.expect("registry ready");
    let err = engine.post_init(&system()).await.expect_err("unknown type");
    let message = format!("{err:#}");
    assert!(
        message.contains("built-in policies rejected") && message.contains("RESOURCE_TYPE_UNKNOWN"),
        "{message}"
    );
}
