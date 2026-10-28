//! The publication boundary: the scoped client is bound only once the registry has
//! admitted the instance, and never by `init` or a failed publication.

use std::sync::Arc;

use authn_resolver_sdk::{AuthNResolverPluginClient, AuthNResolverPluginSpecV1};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::contracts::{Gear, SystemCapability};
use toolkit::gts::PluginV1;
use toolkit::runtime::{GearManager, GrpcInstallerStore, SystemContext};
use toolkit::{ConfigProvider, GearCtx};
use types_registry_sdk::AdmissionFailureReason;
use types_registry_sdk::testing_platform::{Call, MockTypesRegistry};

use super::StaticAuthNPlugin;

const INSTANCE: &str = "cf.builtin.static_authn_resolver.plugin.v1";

struct FixedConfig(Value);

impl ConfigProvider for FixedConfig {
    fn get_gear_config(&self, _gear: &str) -> Option<&Value> {
        Some(&self.0)
    }
}

fn instance_id() -> String {
    PluginV1::<AuthNResolverPluginSpecV1>::build_registration(INSTANCE, "", 0)
        .unwrap()
        .0
        .as_ref()
        .to_owned()
}

fn bound(hub: &ClientHub) -> bool {
    hub.try_get_scoped::<dyn AuthNResolverPluginClient>(&ClientScope::gts_id(&instance_id()))
        .is_some()
}

fn system_context() -> SystemContext {
    SystemContext::new(
        uuid::Uuid::new_v4(),
        Arc::new(GearManager::new()),
        Arc::new(GrpcInstallerStore::new()),
    )
}

/// Runs `init` against `registry` with `config`, and returns the gear and its hub.
async fn initialized(
    registry: Option<&Arc<MockTypesRegistry>>,
    config: Value,
    cancel: CancellationToken,
) -> (StaticAuthNPlugin, Arc<ClientHub>) {
    let hub = Arc::new(ClientHub::new());
    if let Some(registry) = registry {
        registry.install(&hub);
    }
    let ctx = GearCtx::new(
        "static-authn-plugin",
        uuid::Uuid::new_v4(),
        Arc::new(FixedConfig(json!({ "config": config }))),
        Arc::clone(&hub),
        cancel,
    );
    let gear = StaticAuthNPlugin::default();
    gear.init(&ctx).await.expect("init");
    (gear, hub)
}

#[tokio::test]
async fn init_neither_publishes_nor_binds() {
    let registry = Arc::new(MockTypesRegistry::new());
    let (_gear, hub) = initialized(Some(&registry), json!({}), CancellationToken::new()).await;

    assert!(!bound(&hub));
    assert_eq!(registry.calls(Call::Register), 0);
    assert!(registry.content(&instance_id()).is_none());
}

#[tokio::test]
async fn an_admitted_instance_binds_the_client() {
    let registry = Arc::new(MockTypesRegistry::new());
    let config = json!({ "vendor": "cf", "priority": 7 });
    let (gear, hub) = initialized(Some(&registry), config, CancellationToken::new()).await;

    gear.post_init(&system_context()).await.expect("published");

    assert!(bound(&hub));
    let content = registry.content(&instance_id()).expect("admitted");
    assert_eq!(content["vendor"], "cf");
    assert_eq!(content["priority"], 7);
}

/// A later boot under another configuration updates the durable instance.
#[tokio::test]
async fn a_changed_configuration_updates_the_instance() {
    let registry = Arc::new(MockTypesRegistry::new());
    let (_, previous) =
        PluginV1::<AuthNResolverPluginSpecV1>::build_registration(INSTANCE, "old", 1).unwrap();
    registry.seed(&instance_id(), previous);
    let config = json!({ "vendor": "cf", "priority": 7 });
    let (gear, hub) = initialized(Some(&registry), config, CancellationToken::new()).await;

    gear.post_init(&system_context()).await.expect("published");

    assert!(bound(&hub));
    let content = registry.content(&instance_id()).expect("stored");
    assert_eq!(
        (&content["vendor"], &content["priority"]),
        (&json!("cf"), &json!(7))
    );
}

#[tokio::test]
async fn a_refused_instance_fails_boot_and_binds_nothing() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.reject(
        &instance_id(),
        AdmissionFailureReason::IncompatibleWithBaseline,
    );
    let (gear, hub) = initialized(Some(&registry), json!({}), CancellationToken::new()).await;

    let err = gear
        .post_init(&system_context())
        .await
        .expect_err("refused");

    assert!(err.to_string().contains("publishing instance"), "{err:#}");
    assert!(!bound(&hub));
}

/// An admission that never completes runs out the shared startup deadline rather than
/// binding early.
#[tokio::test(start_paused = true)]
async fn a_pending_instance_fails_boot_at_the_deadline_and_binds_nothing() {
    let registry = Arc::new(MockTypesRegistry::new().completing_after(u32::MAX));
    let (gear, hub) = initialized(Some(&registry), json!({}), CancellationToken::new()).await;

    let started = tokio::time::Instant::now();
    gear.post_init(&system_context())
        .await
        .expect_err("pending past the deadline");

    assert_eq!(
        started.elapsed(),
        types_registry_sdk::STARTUP_PUBLICATION_DEADLINE,
        "gives up at the shared deadline, neither earlier nor later"
    );
    assert!(registry.calls(Call::Register) > 0);
    assert!(!bound(&hub));
}

#[tokio::test]
async fn a_cancelled_boot_binds_nothing() {
    let registry = Arc::new(MockTypesRegistry::new());
    let cancel = CancellationToken::new();
    let (gear, hub) = initialized(Some(&registry), json!({}), cancel.clone()).await;
    cancel.cancel();

    gear.post_init(&system_context())
        .await
        .expect_err("cancelled");

    assert!(!bound(&hub));
}

/// No registry API in the hub — the registry is neither linked with a database nor wired
/// remotely: boot fails rather than binding an unpublished plugin.
#[tokio::test]
async fn without_the_platform_api_boot_fails_and_binds_nothing() {
    let (gear, hub) = initialized(None, json!({}), CancellationToken::new()).await;

    let err = gear.post_init(&system_context()).await.expect_err("no API");

    assert!(
        err.to_string().contains("PlatformTypesRegistryApi"),
        "{err:#}"
    );
    assert!(!bound(&hub));
}
