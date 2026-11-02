//! The publication boundary: the runtime publishes the configured instance
//! before `init`; `init` binds the scoped client to the admitted manifest.

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverPluginClient, AuthZResolverPluginSpecV1};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::Gear;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::config::ConfigProvider;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::{
    GtsPublicationCapability, GtsPublicationError, PreparedGtsPublication,
};
use types_registry_sdk::AdmissionFailureReason;
use types_registry_sdk::testing_platform::{Call, MockTypesRegistry};
use uuid::Uuid;

use super::StaticAuthZPlugin;

const SEGMENT: &str = "cf.builtin.static_authz_resolver.plugin.v1";

struct Config(serde_json::Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.0.get(gear)
    }
}

fn config(priority: i16) -> Config {
    Config(json!({
        StaticAuthZPlugin::MODULE_NAME: { "config": { "vendor": "acme", "priority": priority } }
    }))
}

fn ctx(hub: &Arc<ClientHub>, priority: i16) -> GearCtx {
    GearCtx::new(
        StaticAuthZPlugin::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(config(priority)),
        Arc::clone(hub),
        CancellationToken::new(),
    )
}

fn instance(priority: i16) -> (String, serde_json::Value) {
    let (id, content) =
        PluginV1::<AuthZResolverPluginSpecV1>::build_registration(SEGMENT, "acme", priority)
            .expect("plugin instance serializes");
    (id.as_ref().to_owned(), content)
}

fn host() -> (Arc<ClientHub>, Arc<MockTypesRegistry>) {
    let hub = Arc::new(ClientHub::new());
    let registry = Arc::new(MockTypesRegistry::new());
    registry.install(&hub);
    (hub, registry)
}

fn bound(hub: &ClientHub, id: &str) -> bool {
    hub.try_get_scoped::<dyn AuthZResolverPluginClient>(&ClientScope::gts_id(id))
        .is_some()
}

/// What the runtime does under `cancel`: publish the configured manifest
/// until admitted, then `init` with the admitted manifest.
async fn boot(
    hub: &Arc<ClientHub>,
    priority: i16,
    cancel: &CancellationToken,
) -> Result<(), GtsPublicationError> {
    let gear = StaticAuthZPlugin::default();
    let manifest = Arc::new(
        gear.descriptor()
            .build_gts_manifest(&config(priority))
            .unwrap(),
    );
    PreparedGtsPublication::bind(gear.descriptor(), Arc::clone(&manifest), hub)?
        .publish(cancel, Arc::new(|_: &str| {}))
        .await?;
    assert!(
        !bound(hub, &instance(priority).0),
        "nothing bound before init"
    );
    gear.init(&ctx(hub, priority).with_gts_publication_manifest(manifest))
        .await
        .expect("init after admission");
    Ok(())
}

/// Without an admitted manifest — outside the runtime — `init` fails and
/// binds nothing.
#[tokio::test]
async fn init_without_an_admitted_manifest_binds_nothing() {
    let hub = Arc::new(ClientHub::new());

    let err = StaticAuthZPlugin::default()
        .init(&ctx(&hub, 7))
        .await
        .unwrap_err();

    assert!(
        format!("{err:#}").contains("admitted GTS manifest"),
        "{err:#}"
    );
    assert!(!bound(&hub, &instance(7).0));
}

/// The manifest is the configured instance, without `init`.
#[test]
fn the_manifest_is_the_configured_instance() {
    let gear = StaticAuthZPlugin::default();
    let manifest = gear.descriptor().build_gts_manifest(&config(7)).unwrap();

    assert_eq!(manifest.documents(), [instance(7)]);
    assert_eq!(manifest.publisher_name(), "static-authz-plugin");
    assert_eq!(manifest.publisher_version(), env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn publishes_its_instance_before_binding_its_client() {
    let (hub, registry) = host();
    let (id, content) = instance(7);

    boot(&hub, 7, &CancellationToken::new()).await.unwrap();

    assert_eq!(registry.content(&id), Some(content));
    assert!(bound(&hub, &id));
}

#[tokio::test]
async fn an_equal_restart_submits_nothing() {
    let (hub, registry) = host();
    let (id, content) = instance(7);
    registry.seed(&id, content);

    boot(&hub, 7, &CancellationToken::new()).await.unwrap();

    assert_eq!(registry.calls(Call::Register), 0);
    assert!(bound(&hub, &id));
}

#[tokio::test]
async fn a_changed_priority_updates_the_stored_instance() {
    let (hub, registry) = host();
    let (id, before) = instance(7);
    registry.seed(&id, before);

    boot(&hub, 3, &CancellationToken::new()).await.unwrap();

    assert_eq!(registry.content(&id), Some(instance(3).1));
}

/// A refusal is no success: nothing is bound.
#[tokio::test]
async fn a_rejected_instance_binds_nothing() {
    let (hub, registry) = host();
    let (id, _) = instance(7);
    registry.reject(&id, AdmissionFailureReason::InvalidValue);

    let err = boot(&hub, 7, &CancellationToken::new()).await.unwrap_err();

    assert!(matches!(err, GtsPublicationError::Refused { .. }), "{err}");
    assert!(err.to_string().contains(&id), "{err}");
    assert!(!bound(&hub, &id));
}

/// An admission that never completes keeps retrying past the startup deadline:
/// an accepted but pending instance is never selectable.
#[tokio::test(start_paused = true)]
async fn a_pending_instance_keeps_retrying_without_binding() {
    let hub = Arc::new(ClientHub::new());
    let registry = Arc::new(MockTypesRegistry::new().completing_after(u32::MAX));
    registry.install(&hub);
    let (id, _) = instance(7);

    let cancel = CancellationToken::new();
    let running = tokio::spawn({
        let (hub, cancel) = (Arc::clone(&hub), cancel.clone());
        async move { boot(&hub, 7, &cancel).await }
    });
    tokio::time::sleep(types_registry_sdk::STARTUP_PUBLICATION_DEADLINE * 3).await;

    assert!(!running.is_finished(), "still pending");
    assert!(registry.calls(Call::Register) > 1, "retried");
    assert!(!bound(&hub, &id));
    cancel.cancel();
    assert!(matches!(
        running.await.unwrap(),
        Err(GtsPublicationError::Cancelled { .. })
    ));
    assert!(!bound(&hub, &id));
}

/// The legacy catalogue is not a fallback: without the platform API the
/// publication cannot start.
#[tokio::test]
async fn a_host_without_the_platform_registry_api_cannot_publish() {
    let hub = Arc::new(ClientHub::new());
    let (id, _) = instance(7);

    let err = boot(&hub, 7, &CancellationToken::new()).await.unwrap_err();

    assert!(matches!(err, GtsPublicationError::Backend { .. }), "{err}");
    assert!(!bound(&hub, &id));
}
