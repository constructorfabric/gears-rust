use std::sync::Arc;

use async_trait::async_trait;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use resource_group_sdk::models::{ResourceGroup, ResourceGroupMembership, ResourceGroupWithDepth};
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
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use types_registry_sdk::AdmissionFailureReason;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use uuid::Uuid;

use super::RgTrPlugin;

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

/// Resource Group is resolved at `init()` but not called by publication.
struct Unreachable;

#[async_trait]
impl ResourceGroupReadHierarchy for Unreachable {
    async fn get_group_descendants(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
        Err(CanonicalError::service_unavailable().create())
    }
    async fn get_group_ancestors(
        &self,
        _: &SecurityContext,
        _: Uuid,
        _: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
        Err(CanonicalError::service_unavailable().create())
    }
    async fn list_groups(
        &self,
        _: &SecurityContext,
        _: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, CanonicalError> {
        Err(CanonicalError::service_unavailable().create())
    }
    async fn get_group(
        &self,
        _: &SecurityContext,
        _: Uuid,
    ) -> Result<ResourceGroup, CanonicalError> {
        Err(CanonicalError::service_unavailable().create())
    }
    async fn list_memberships(
        &self,
        _: &SecurityContext,
        _: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, CanonicalError> {
        Err(CanonicalError::service_unavailable().create())
    }
}

fn config() -> Config {
    Config(json!({
        RgTrPlugin::MODULE_NAME: { "config": { "vendor": "acme", "priority": 50 } }
    }))
}

fn ctx(hub: &Arc<ClientHub>) -> GearCtx {
    let rg: Arc<dyn ResourceGroupReadHierarchy> = Arc::new(Unreachable);
    hub.register::<dyn ResourceGroupReadHierarchy>(rg);
    GearCtx::new(
        RgTrPlugin::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(config()),
        Arc::clone(hub),
        CancellationToken::new(),
    )
}

fn instance() -> (String, serde_json::Value) {
    let (id, content) = PluginV1::<TenantResolverPluginSpecV1>::build_registration(
        "cf.builtin.rg_tenant_resolver.plugin.v1",
        "acme",
        50,
    )
    .expect("plugin instance serializes");
    (id.as_ref().to_owned(), content)
}

/// The manifest the runtime publishes, without `init`.
fn manifest() -> GtsManifest {
    RgTrPlugin::default()
        .descriptor()
        .build_gts_manifest(&config())
        .expect("manifest")
}

/// The runtime's publication of the manifest under `cancel`.
async fn published(hub: &ClientHub, cancel: &CancellationToken) -> Result<(), GtsPublicationError> {
    PreparedGtsPublication::bind(
        RgTrPlugin::default().descriptor(),
        Arc::new(manifest()),
        toolkit::gts_publication::GtsPublicationOptions::default(),
        hub,
    )?
    .publish(cancel, Arc::new(|_: &str| {}))
    .await
}

#[test]
fn the_manifest_is_the_configured_instance() {
    assert_eq!(
        manifest().documents(),
        [instance()]
            .map(|(id, document)| (types_registry_sdk::GtsId::try_new(&id).unwrap(), document))
    );
}

// Publication is independent of Resource Group's state (#4568): it needs no `init`.
#[tokio::test]
async fn init_binds_the_admitted_instance() {
    let (hub, registry) = host();
    let (id, content) = instance();

    published(&hub, &CancellationToken::new())
        .await
        .expect("admitted");
    assert!(!backend_published(&hub, &id), "nothing bound before init");
    RgTrPlugin::default()
        .init(&ctx(&hub).with_gts_publication_manifest_for_test(Arc::new(manifest())))
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

    let error = RgTrPlugin::default()
        .init(&ctx(&hub))
        .await
        .expect_err("no admitted manifest");

    assert!(
        format!("{error:#}").contains("admitted GTS manifest"),
        "{error:#}"
    );
    assert!(!backend_published(&hub, &id));
}
