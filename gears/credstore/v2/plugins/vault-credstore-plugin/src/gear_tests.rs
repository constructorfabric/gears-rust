//! The publication boundary: the runtime publishes the configured instance
//! before `init`; `init` binds the scoped client to the admitted manifest.

use std::sync::Arc;

use credstore_sdk::{CredStorePluginClientV2, CredStorePluginSpecV2};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::Gear;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::config::ConfigProvider;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationCapability;
use uuid::Uuid;

use super::VaultCredStorePlugin;

const SEGMENT: &str = "cf.core._.vault_credstore.v1";

struct Config(serde_json::Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.0.get(gear)
    }
}

fn config(priority: i16) -> Config {
    Config(json!({
        VaultCredStorePlugin::MODULE_NAME: { "config": { "vendor": "acme", "priority": priority, "token": "s.token" } }
    }))
}

fn ctx(hub: &Arc<ClientHub>, priority: i16) -> GearCtx {
    GearCtx::new(
        VaultCredStorePlugin::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(config(priority)),
        Arc::clone(hub),
        CancellationToken::new(),
    )
}

fn instance(priority: i16) -> (String, serde_json::Value) {
    let (id, content) =
        PluginV1::<CredStorePluginSpecV2>::build_registration(SEGMENT, "acme", priority)
            .expect("plugin instance serializes");
    (id.as_ref().to_owned(), content)
}

fn bound(hub: &ClientHub, id: &str) -> bool {
    hub.try_get_scoped::<dyn CredStorePluginClientV2>(&ClientScope::gts_id(id))
        .is_some()
}

/// Without an admitted manifest (outside the runtime) `init` fails and binds
/// nothing.
#[tokio::test]
async fn init_without_an_admitted_manifest_binds_nothing() {
    let hub = Arc::new(ClientHub::new());

    let err = VaultCredStorePlugin::default()
        .init(&ctx(&hub, 7))
        .await
        .unwrap_err();

    assert!(
        format!("{err:#}").contains("admitted GTS manifest"),
        "{err:#}"
    );
    assert!(!bound(&hub, &instance(7).0));
}

/// The manifest is the configured instance, built without `init`.
#[test]
fn the_manifest_is_the_configured_instance() {
    let gear = VaultCredStorePlugin::default();
    let manifest = gear.descriptor().build_gts_manifest(&config(7)).unwrap();

    assert_eq!(
        manifest.documents(),
        [instance(7)]
            .map(|(id, document)| (types_registry_sdk::GtsId::try_new(&id).unwrap(), document))
    );
    assert_eq!(manifest.publisher_name(), VaultCredStorePlugin::MODULE_NAME);
    assert_eq!(manifest.publisher_version(), env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn init_binds_the_admitted_instance() {
    let hub = Arc::new(ClientHub::new());
    let gear = VaultCredStorePlugin::default();
    let manifest = Arc::new(gear.descriptor().build_gts_manifest(&config(7)).unwrap());

    gear.init(&ctx(&hub, 7).with_gts_publication_manifest_for_test(manifest))
        .await
        .expect("init with an admitted manifest");

    assert!(bound(&hub, &instance(7).0));
}

/// A second `init` on the same gear is refused.
#[tokio::test]
async fn a_second_init_is_refused() {
    let hub = Arc::new(ClientHub::new());
    let gear = VaultCredStorePlugin::default();
    let manifest = Arc::new(gear.descriptor().build_gts_manifest(&config(7)).unwrap());
    let ctx = ctx(&hub, 7).with_gts_publication_manifest_for_test(manifest);

    gear.init(&ctx).await.expect("first init");
    let err = gear.init(&ctx).await.unwrap_err();

    assert!(
        format!("{err:#}").contains("already initialized"),
        "{err:#}"
    );
}
