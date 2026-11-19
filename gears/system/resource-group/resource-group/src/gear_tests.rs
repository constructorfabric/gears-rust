//! Gear-level tests for [`ResourceGroup`].
#![allow(clippy::expect_used)]

use std::sync::Arc;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::{ClientHub, ConfigProvider, Gear, GearCtx};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use types_registry_sdk::testing_platform::MockTypesRegistry;
use uuid::Uuid;

use super::ResourceGroup;

/// A config provider answering the gear from one `gears.resource-group` section.
struct FixedConfig(Value);

impl ConfigProvider for FixedConfig {
    fn get_gear_config(&self, gear: &str) -> Option<&Value> {
        (gear == ResourceGroup::MODULE_NAME).then_some(&self.0)
    }
}

/// `AuthZ` is a consumed contract, wired after `init` (locally or remotely), so
/// `init` must not look it up. The types registry's tenant API is required.
#[tokio::test]
async fn init_does_not_need_authz_in_the_hub() {
    let dsn = format!(
        "sqlite:file:resource_group_gear_{}?mode=memory&cache=shared",
        Uuid::new_v4()
    );
    let db = connect_db(&dsn, ConnectOpts::default())
        .await
        .expect("connect sqlite");

    let hub = Arc::new(ClientHub::new());
    // The types registry's APIs are wired before the application group.
    Arc::new(MockTypesRegistry::new()).install(&hub);

    let ctx = GearCtx::new(
        ResourceGroup::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(FixedConfig(json!({ "config": {} }))),
        hub,
        CancellationToken::default(),
    )
    .with_db(DBProvider::new(db));

    ResourceGroup::default()
        .init(&ctx)
        .await
        .expect("init without AuthZ in the hub");
}

struct NoConfig;

impl ConfigProvider for NoConfig {
    fn get_gear_config(&self, _gear: &str) -> Option<&Value> {
        None
    }
}

/// RG publishes exactly its SDK's declarations (the RG base type), under its
/// own name and version, before any application `init`.
#[test]
fn the_publication_manifest_is_the_sdk_declarations() {
    use toolkit::gts_publication::GtsPublicationCapability as _;

    let manifest = ResourceGroup::default()
        .descriptor()
        .build_gts_manifest(&NoConfig)
        .expect("manifest");

    assert_eq!(manifest.publisher_name(), ResourceGroup::MODULE_NAME);
    assert_eq!(manifest.publisher_version(), env!("CARGO_PKG_VERSION"));
    let ids: Vec<&str> = manifest.documents().iter().map(|(id, _)| id.id()).collect();
    assert_eq!(ids, [toolkit_gts::gts_id!("cf.core.rg.type.v1~")]);
}

/// Without the types registry's tenant API `init` fails closed: RG keeps no
/// type catalogue of its own to fall back on.
#[tokio::test]
async fn init_requires_the_types_registry_api() {
    let dsn = format!(
        "sqlite:file:resource_group_gear_{}?mode=memory&cache=shared",
        Uuid::new_v4()
    );
    let db = connect_db(&dsn, ConnectOpts::default())
        .await
        .expect("connect sqlite");
    let ctx = GearCtx::new(
        ResourceGroup::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(FixedConfig(json!({ "config": {} }))),
        Arc::new(ClientHub::new()),
        CancellationToken::default(),
    )
    .with_db(DBProvider::new(db));

    let err = ResourceGroup::default()
        .init(&ctx)
        .await
        .expect_err("no types registry API");
    assert!(format!("{err:#}").contains("TypesRegistryApi"), "{err:#}");
}
