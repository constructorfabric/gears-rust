use std::sync::Arc;

use serde_json::json;
use tenant_resolver_sdk::{TenantResolverPluginClient, TenantResolverPluginSpecV1};
use tokio_util::sync::CancellationToken;
use toolkit::Gear;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::config::ConfigProvider;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use types_registry_sdk::AdmissionFailureReason;
use types_registry_sdk::testing_platform::{Call, MockTypesRegistry};
use uuid::Uuid;

use super::SingleTenantTrPlugin;

struct Config(serde_json::Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.0.get(gear)
    }
}

fn host() -> (Arc<ClientHub>, Arc<MockTypesRegistry>) {
    let hub = Arc::new(ClientHub::new());
    let registry = Arc::new(MockTypesRegistry::new());
    registry.install(&hub);
    (hub, registry)
}

fn backend_published(hub: &ClientHub, id: &str) -> bool {
    hub.try_get_scoped::<dyn TenantResolverPluginClient>(&ClientScope::gts_id(id))
        .is_some()
}

fn ctx(hub: &Arc<ClientHub>) -> GearCtx {
    GearCtx::new(
        SingleTenantTrPlugin::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(Config(json!({
            SingleTenantTrPlugin::MODULE_NAME: { "config": { "vendor": "acme", "priority": 5 } }
        }))),
        Arc::clone(hub),
        CancellationToken::new(),
    )
}

fn instance() -> (String, serde_json::Value) {
    let (id, content) = PluginV1::<TenantResolverPluginSpecV1>::build_registration(
        "cf.builtin.single_tenant_resolver.plugin.v1",
        "acme",
        5,
    )
    .expect("plugin instance serializes");
    (id.as_ref().to_owned(), content)
}

#[tokio::test]
async fn publishes_its_instance_before_its_backend() {
    let (hub, registry) = host();
    let (id, content) = instance();

    SingleTenantTrPlugin
        .init(&ctx(&hub))
        .await
        .expect("init succeeds");

    assert_eq!(registry.content(&id), Some(content));
    assert!(backend_published(&hub, &id));
}

#[tokio::test]
async fn an_equal_restart_submits_nothing() {
    let (hub, registry) = host();
    let (id, content) = instance();
    registry.seed(&id, content);

    SingleTenantTrPlugin
        .init(&ctx(&hub))
        .await
        .expect("init succeeds");

    assert_eq!(registry.calls(Call::Register), 0);
    assert!(backend_published(&hub, &id));
}

#[tokio::test]
async fn a_rejected_instance_fails_init_without_publishing_the_backend() {
    let (hub, registry) = host();
    let (id, _) = instance();
    registry.reject(&id, AdmissionFailureReason::InvalidValue);

    let error = SingleTenantTrPlugin
        .init(&ctx(&hub))
        .await
        .expect_err("a rejected instance fails boot");

    assert!(error.to_string().contains(&id), "{error:#}");
    assert!(!backend_published(&hub, &id));
}
