use std::sync::Arc;

use account_management_sdk::{IdpPluginClient, IdpPluginSpecV1};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::Gear;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::config::ConfigProvider;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use types_registry_sdk::AdmissionFailureReason;
use types_registry_sdk::testing_platform::{Call, MockTypesRegistry};
use uuid::Uuid;

use super::StaticIdpPlugin;

struct Config(serde_json::Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.0.get(gear)
    }
}

fn ctx(hub: &Arc<ClientHub>, priority: i16) -> GearCtx {
    GearCtx::new(
        StaticIdpPlugin::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(Config(json!({
            StaticIdpPlugin::MODULE_NAME: { "config": { "vendor": "cf", "priority": priority } }
        }))),
        Arc::clone(hub),
        CancellationToken::new(),
    )
}

fn instance(priority: i16) -> (String, serde_json::Value) {
    let (id, content) = PluginV1::<IdpPluginSpecV1>::build_registration(
        "cf.builtin.static_idp.plugin.v1",
        "cf",
        priority,
    )
    .expect("plugin instance serializes");
    (id.as_ref().to_owned(), content)
}

fn host() -> (Arc<ClientHub>, Arc<MockTypesRegistry>) {
    let hub = Arc::new(ClientHub::new());
    let registry = Arc::new(MockTypesRegistry::new());
    registry.install(&hub);
    (hub, registry)
}

fn backend_published(hub: &ClientHub, id: &str) -> bool {
    hub.try_get_scoped::<dyn IdpPluginClient>(&ClientScope::gts_id(id))
        .is_some()
}

#[tokio::test]
async fn publishes_its_instance_before_its_backend() {
    let (hub, registry) = host();
    let (id, content) = instance(100);

    StaticIdpPlugin::default()
        .init(&ctx(&hub, 100))
        .await
        .expect("init succeeds");

    assert_eq!(registry.content(&id), Some(content));
    assert!(backend_published(&hub, &id));
}

#[tokio::test]
async fn an_equal_restart_submits_nothing_and_a_changed_priority_updates() {
    let (hub, registry) = host();
    let (id, content) = instance(100);
    registry.seed(&id, content);

    StaticIdpPlugin::default()
        .init(&ctx(&hub, 100))
        .await
        .expect("equal restart");
    assert_eq!(registry.calls(Call::Register), 0);

    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    StaticIdpPlugin::default()
        .init(&ctx(&hub, 7))
        .await
        .expect("changed priority");
    assert_eq!(registry.content(&id), Some(instance(7).1));
}

#[tokio::test]
async fn a_rejected_instance_fails_init_without_publishing_the_backend() {
    let (hub, registry) = host();
    let (id, _) = instance(100);
    registry.reject(&id, AdmissionFailureReason::InvalidValue);

    let error = StaticIdpPlugin::default()
        .init(&ctx(&hub, 100))
        .await
        .expect_err("a rejected instance fails boot");

    assert!(error.to_string().contains(&id), "{error:#}");
    assert!(!backend_published(&hub, &id));
}

#[tokio::test]
async fn a_host_without_the_platform_registry_api_fails_init() {
    let hub = Arc::new(ClientHub::new());
    let (id, _) = instance(100);

    StaticIdpPlugin::default()
        .init(&ctx(&hub, 100))
        .await
        .expect_err("no registry, no fallback");

    assert!(!backend_published(&hub, &id));
}
