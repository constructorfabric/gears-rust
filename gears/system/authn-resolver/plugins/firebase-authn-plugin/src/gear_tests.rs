//! The plugin must publish into the platform catalogue the migrated resolver reads.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use authn_resolver_sdk::{AuthNResolverPluginClient, AuthNResolverPluginSpecV1};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::gts::PluginV1;
use toolkit::gts_publication::{GtsManifest, PreparedGtsPublication};
use toolkit::registry::{GearRegistry, GtsPublicationCap};
use toolkit::{ConfigProvider, Gear, GearCtx};
use toolkit_security::PlatformSecurityContext;
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::TypesRegistryService;
use types_registry::domain::legacy_local_client::TypesRegistryLocalClient;
use types_registry::infra::InMemoryGtsRepository;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use types_registry_sdk::{
    AdmissionFailureReason, GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApiExt,
};

use super::{FirebaseAuthnPlugin, PLUGIN_SUFFIX};

struct Config(Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, _gear: &str) -> Option<&Value> {
        Some(&self.0)
    }
}

fn config(vendor: &str, priority: i16) -> Config {
    Config(json!({ "config": {
        "vendor": vendor,
        "priority": priority,
        "project_id": "firebase-authn-test",
        "identities": [{
            "uid": "account-a", "email": "a@example.test",
            "tenant_id": "00000000-0000-0000-0000-000000000001",
            "token_scopes": ["scope:a"]
        }]
    }}))
}

fn instance_id() -> String {
    PluginV1::<AuthNResolverPluginSpecV1>::build_registration(PLUGIN_SUFFIX, "", 0)
        .unwrap()
        .0
        .to_string()
}

fn bound(hub: &ClientHub) -> bool {
    hub.try_get_scoped::<dyn AuthNResolverPluginClient>(&ClientScope::gts_id(&instance_id()))
        .is_some()
}

fn hub(registry: &Arc<MockTypesRegistry>) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    // The transitional registry still serves the old API. Its presence must not
    // make a plugin visible only in that separate, process-local catalogue.
    let cfg = TypesRegistryConfig::default();
    let service = Arc::new(TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(cfg.to_gts_config())),
        cfg,
    ));
    hub.register::<dyn types_registry_sdk::TypesRegistryClient>(Arc::new(
        TypesRegistryLocalClient::new(service),
    ));
    hub
}

fn ctx(hub: Arc<ClientHub>, config: Config, manifest: Option<GtsManifest>) -> GearCtx {
    let ctx = GearCtx::new(
        "firebase-authn-plugin",
        uuid::Uuid::new_v4(),
        Arc::new(config),
        hub,
        CancellationToken::new(),
    );
    match manifest {
        Some(manifest) => ctx.with_gts_publication_manifest_for_test(Arc::new(manifest)),
        None => ctx,
    }
}

/// Run the same optional capability boundary as the runtime, before `init`.
async fn prepare(hub: &ClientHub, config: &Config) -> anyhow::Result<Option<GtsManifest>> {
    let gears = GearRegistry::discover_and_build()?;
    let entry = gears
        .gears()
        .iter()
        .find(|g| g.name() == "firebase-authn-plugin")
        .expect("linked Firebase plugin");
    let Some(publication) = entry.caps().query::<GtsPublicationCap>() else {
        return Ok(None);
    };
    let descriptor = publication.descriptor();
    let manifest = descriptor.build_gts_manifest(config)?;
    PreparedGtsPublication::prepare(descriptor, config, hub)?
        .publish(&CancellationToken::new(), Arc::new(|_: &str| {}))
        .await?;
    Ok(Some(manifest))
}

#[tokio::test]
async fn the_instance_is_discoverable_in_the_platform_catalogue_after_publication() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub(&registry);
    let config = config("cf", 7);
    let manifest = prepare(&hub, &config).await.unwrap();
    FirebaseAuthnPlugin::default()
        .init(&ctx(Arc::clone(&hub), config, manifest))
        .await
        .unwrap();

    let listed = registry
        .list_instances(
            &PlatformSecurityContext::outbound_marker(),
            ListEntitiesRequest::matching(
                GtsIdPattern::try_new(AuthNResolverPluginSpecV1::gts_type_id().as_ref()).unwrap(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        listed.len(),
        1,
        "the migrated resolver reads this catalogue"
    );
    let content = listed[0].content.as_ref().unwrap();
    assert_eq!(content["vendor"], "cf");
    assert_eq!(content["priority"], 7);
    assert!(content.get("identities").is_none());
    assert!(content.get("project_id").is_none());
    let selected = toolkit::plugins::choose_registered_plugin_instance::<
        AuthNResolverPluginSpecV1,
        dyn AuthNResolverPluginClient,
    >(
        &hub,
        "cf",
        listed.iter().map(|i| (i.id.as_ref(), i.content.as_ref())),
    )
    .unwrap();
    assert_eq!(selected, instance_id());
}

#[tokio::test]
async fn a_refused_instance_never_binds_a_client() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.reject(
        &instance_id(),
        AdmissionFailureReason::IncompatibleWithBaseline,
    );
    let hub = hub(&registry);
    let config = config("cf", 7);
    let publication = prepare(&hub, &config).await;
    assert!(
        publication.is_err(),
        "refused instance must fail preparation"
    );
    assert!(!bound(&hub));
}

#[tokio::test]
async fn init_without_an_admitted_manifest_binds_nothing_even_with_the_legacy_api() {
    let hub = hub(&Arc::new(MockTypesRegistry::new()));
    let result = FirebaseAuthnPlugin::default()
        .init(&ctx(Arc::clone(&hub), config("cf", 7), None))
        .await;
    assert!(result.is_err(), "init requires an admitted manifest");
    assert!(!bound(&hub));
}

#[tokio::test]
async fn a_changed_configuration_updates_the_same_instance() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub(&registry);
    prepare(&hub, &config("previous", 50)).await.unwrap();
    prepare(&hub, &config("next", 7)).await.unwrap();

    let content = registry.content(&instance_id()).unwrap();
    assert_eq!(content["vendor"], "next");
    assert_eq!(content["priority"], 7);
}

#[tokio::test]
async fn invalid_identity_configuration_publishes_nothing() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub(&registry);
    let mut config = config("cf", 7);
    config.0["config"]["identities"] = json!([]);

    let error = prepare(&hub, &config).await.unwrap_err();

    assert!(format!("{error:#}").contains("identity mappings are required"));
    assert!(registry.content(&instance_id()).is_none());
    assert!(!bound(&hub));
}
