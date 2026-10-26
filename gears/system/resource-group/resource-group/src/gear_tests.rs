//! Gear-level tests for [`ResourceGroup`].
#![allow(clippy::expect_used)]

use std::sync::Arc;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::{ClientHub, ConfigProvider, Gear, GearCtx};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use types_registry_sdk::TypesRegistryClient;
use types_registry_sdk::testing::MockTypesRegistryClient;
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
/// `init` must not look it up.
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
    hub.register::<dyn TypesRegistryClient>(Arc::new(MockTypesRegistryClient::new()));

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
