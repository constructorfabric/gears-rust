#![allow(clippy::expect_used, clippy::unwrap_used)]

//! What the gear publishes to types-registry, seeded by the real registry.
//!
//! The gear submits its base ontology and its permissions to the `toolkit-gts`
//! inventory (`src/gts.rs`), and types-registry admits the whole inventory at
//! its own startup. One entry it refuses stops that startup -- and the
//! platform with it -- so the entries are proved here against the registry
//! itself rather than against what its rules are believed to be: every one
//! is admitted, and every base schema is stored exactly as the gear validates
//! against it.

use std::sync::Arc;

use graph_storage::domain::ontology::BASE_SCHEMAS;
use toolkit::config::ConfigProvider;
use toolkit::contracts::{DatabaseCapability, SystemCapability};
use toolkit::runtime::{GearManager, GrpcInstallerStore, SystemContext};
use toolkit::{ClientHub, Gear, GearCtx};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use types_registry::gear::TypesRegistryGear;
use types_registry_sdk::TypesRegistryClient;
use uuid::Uuid;

/// The permissions `src/gts.rs` declares, one per resource and action the PEP
/// checks.
const PERMISSIONS: [(&str, &str, &str); 5] = [
    ("node_read", graph_storage_sdk::gts::NODE_RESOURCE, "read"),
    ("node_write", graph_storage_sdk::gts::NODE_RESOURCE, "write"),
    (
        "node_delete",
        graph_storage_sdk::gts::NODE_RESOURCE,
        "delete",
    ),
    ("type_read", graph_storage_sdk::gts::TYPE_RESOURCE, "read"),
    ("type_admin", graph_storage_sdk::gts::TYPE_RESOURCE, "admin"),
];

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

/// The registry, initialised and seeded: its `post_init` admits the inventory
/// and fails when any entry is refused.
async fn seeded_registry(hub: &Arc<ClientHub>) -> TypesRegistryGear {
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
    .expect("an in-memory database");
    run_migrations_for_testing(&db, gear.migrations())
        .await
        .expect("the registry's migrations apply");
    let ctx = GearCtx::new(
        "types-registry",
        Uuid::new_v4(),
        Arc::new(NoConfig),
        Arc::clone(hub),
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db(DBProvider::new(db));
    gear.init(&ctx).await.expect("types-registry init");
    gear.post_init(&system())
        .await
        .expect("the registry admits the whole inventory, this gear's entries included");
    gear
}

#[tokio::test]
async fn the_registry_admits_the_base_ontology_and_the_permissions_as_published() {
    let hub = Arc::new(ClientHub::new());
    let _registry = seeded_registry(&hub).await;
    let client = hub
        .get::<dyn TypesRegistryClient>()
        .expect("the registry client is wired");

    for (type_id, raw) in BASE_SCHEMAS {
        let stored = client
            .get_type_schema(type_id)
            .await
            .unwrap_or_else(|error| panic!("{type_id} is in the registry: {error}"));
        let published: serde_json::Value =
            serde_json::from_str(raw).expect("the embedded schema is JSON");
        assert_eq!(
            stored.raw_schema, published,
            "{type_id}: the registry holds the schema the gear validates against"
        );
    }

    for (name, resource, action) in PERMISSIONS {
        let id = format!("gts.cf.toolkit.authz.permission.v1~cf.core.graph.{name}.v1");
        let instance = client
            .get_instance(&id)
            .await
            .unwrap_or_else(|error| panic!("{id} is in the registry: {error}"));
        assert_eq!(instance.object["resource_type"], resource, "{id}");
        assert_eq!(instance.object["action"], action, "{id}");
    }
}
