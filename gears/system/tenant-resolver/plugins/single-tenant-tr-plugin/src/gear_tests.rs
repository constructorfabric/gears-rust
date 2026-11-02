use std::sync::Arc;

use serde_json::json;
use tenant_resolver_sdk::{TenantResolverPluginClient, TenantResolverPluginSpecV1};
use tokio_util::sync::CancellationToken;
use toolkit::Gear;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::config::ConfigProvider;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::{
    GtsManifest, GtsPublicationCapability as _, GtsPublicationError, PreparedGtsPublication,
};
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

fn config() -> Config {
    Config(json!({
        SingleTenantTrPlugin::MODULE_NAME: { "config": { "vendor": "acme", "priority": 5 } }
    }))
}

fn ctx(hub: &Arc<ClientHub>) -> GearCtx {
    GearCtx::new(
        SingleTenantTrPlugin::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(config()),
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

/// The manifest the runtime publishes, without `init`.
fn manifest() -> GtsManifest {
    SingleTenantTrPlugin
        .descriptor()
        .build_gts_manifest(&config())
        .expect("manifest")
}

/// The runtime's publication of the manifest under `cancel`.
async fn published(hub: &ClientHub, cancel: &CancellationToken) -> Result<(), GtsPublicationError> {
    PreparedGtsPublication::bind(SingleTenantTrPlugin.descriptor(), Arc::new(manifest()), hub)?
        .publish(cancel, Arc::new(|_: &str| {}))
        .await
}

#[test]
fn the_manifest_is_the_configured_instance() {
    assert_eq!(manifest().documents(), [instance()]);
}

// Publication is independent of the plugin's runtime dependencies: it needs no `init`.
#[tokio::test]
async fn init_binds_the_admitted_instance() {
    let (hub, registry) = host();
    let (id, content) = instance();

    published(&hub, &CancellationToken::new())
        .await
        .expect("admitted");
    assert!(!backend_published(&hub, &id), "nothing bound before init");
    SingleTenantTrPlugin
        .init(&ctx(&hub).with_gts_publication_manifest(Arc::new(manifest())))
        .await
        .expect("init succeeds");

    assert_eq!(registry.content(&id), Some(content));
    assert!(backend_published(&hub, &id));
}

/// A refusal fails the publication: the runtime never calls `init`.
#[tokio::test]
async fn a_rejected_instance_fails_the_publication() {
    let (hub, registry) = host();
    let (id, _) = instance();
    registry.reject(&id, AdmissionFailureReason::InvalidValue);

    let error = published(&hub, &CancellationToken::new())
        .await
        .expect_err("a rejected instance");

    assert!(
        matches!(error, GtsPublicationError::Refused { .. }),
        "{error}"
    );
    assert!(error.to_string().contains(&id), "{error:#}");
}

/// Outside the runtime there is no admitted manifest: nothing is bound.
#[tokio::test]
async fn init_without_an_admitted_manifest_binds_nothing() {
    let (hub, _registry) = host();
    let (id, _) = instance();

    let error = SingleTenantTrPlugin
        .init(&ctx(&hub))
        .await
        .expect_err("no admitted manifest");

    assert!(
        format!("{error:#}").contains("admitted GTS manifest"),
        "{error:#}"
    );
    assert!(!backend_published(&hub, &id));
}

#[tokio::test]
async fn an_equal_restart_submits_nothing() {
    let (hub, registry) = host();
    let (id, content) = instance();
    registry.seed(&id, content);

    published(&hub, &CancellationToken::new())
        .await
        .expect("admitted");

    assert_eq!(registry.calls(Call::Register), 0);
}
