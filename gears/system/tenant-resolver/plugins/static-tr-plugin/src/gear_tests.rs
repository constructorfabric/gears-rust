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

use super::StaticTrPlugin;

const SEGMENT: &str = "cf.builtin.static_tenant_resolver.plugin.v1";

struct Config(serde_json::Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.0.get(gear)
    }
}

fn ctx(hub: &Arc<ClientHub>, priority: i16, cancel: CancellationToken) -> GearCtx {
    GearCtx::new(
        StaticTrPlugin::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(Config(json!({
            StaticTrPlugin::MODULE_NAME: { "config": {
                "vendor": "acme",
                "priority": priority,
                "tenants": [{ "id": Uuid::from_u128(1), "name": "root" }],
            } }
        }))),
        Arc::clone(hub),
        cancel,
    )
}

fn instance(priority: i16) -> (String, serde_json::Value) {
    let (id, content) =
        PluginV1::<TenantResolverPluginSpecV1>::build_registration(SEGMENT, "acme", priority)
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
    hub.try_get_scoped::<dyn TenantResolverPluginClient>(&ClientScope::gts_id(id))
        .is_some()
}

#[tokio::test]
async fn publishes_its_instance_before_its_backend() {
    let (hub, registry) = host();
    let (id, content) = instance(7);

    StaticTrPlugin::default()
        .init(&ctx(&hub, 7, CancellationToken::new()))
        .await
        .expect("init succeeds");

    assert_eq!(registry.content(&id), Some(content));
    assert!(backend_published(&hub, &id));
}

#[tokio::test]
async fn an_equal_restart_submits_nothing() {
    let (hub, registry) = host();
    let (id, content) = instance(7);
    registry.seed(&id, content);

    StaticTrPlugin::default()
        .init(&ctx(&hub, 7, CancellationToken::new()))
        .await
        .expect("init succeeds");

    assert_eq!(registry.calls(Call::Register), 0);
    assert!(backend_published(&hub, &id));
}

#[tokio::test]
async fn a_changed_priority_updates_the_stored_instance() {
    let (hub, registry) = host();
    let (id, before) = instance(7);
    registry.seed(&id, before);

    StaticTrPlugin::default()
        .init(&ctx(&hub, 3, CancellationToken::new()))
        .await
        .expect("init succeeds");

    assert_eq!(registry.content(&id), Some(instance(3).1));
}

#[tokio::test]
async fn a_rejected_instance_fails_init_without_publishing_the_backend() {
    let (hub, registry) = host();
    let (id, _) = instance(7);
    registry.reject(&id, AdmissionFailureReason::InvalidValue);

    let error = StaticTrPlugin::default()
        .init(&ctx(&hub, 7, CancellationToken::new()))
        .await
        .expect_err("a rejected instance fails boot");

    assert!(error.to_string().contains(&id), "{error:#}");
    assert!(!backend_published(&hub, &id));
}

#[tokio::test]
async fn cancellation_fails_init_without_publishing_the_backend() {
    let (hub, _registry) = host();
    let (id, _) = instance(7);
    let cancel = CancellationToken::new();
    cancel.cancel();

    StaticTrPlugin::default()
        .init(&ctx(&hub, 7, cancel))
        .await
        .expect_err("a cancelled publication fails boot");

    assert!(!backend_published(&hub, &id));
}

#[tokio::test]
async fn a_host_without_the_platform_registry_api_fails_init() {
    let hub = Arc::new(ClientHub::new());
    let (id, _) = instance(7);

    StaticTrPlugin::default()
        .init(&ctx(&hub, 7, CancellationToken::new()))
        .await
        .expect_err("no registry, no fallback");

    assert!(!backend_published(&hub, &id));
}
