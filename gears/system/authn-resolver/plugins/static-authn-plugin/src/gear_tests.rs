//! The publication boundary: the runtime publishes the config-only instance
//! before `init`, and `init` binds the scoped client to the admitted
//! manifest. Nothing binds without an admitted manifest.

use std::sync::Arc;

use authn_resolver_sdk::{AuthNResolverPluginClient, AuthNResolverPluginSpecV1};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::contracts::Gear;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::{
    GtsManifest, GtsPublicationCapability as _, GtsPublicationError, PreparedGtsPublication,
};
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

fn config_of(config: &Value) -> FixedConfig {
    FixedConfig(json!({ "config": config }))
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

/// The manifest the runtime publishes for `config`, before any `init`.
fn manifest(config: &Value) -> GtsManifest {
    StaticAuthNPlugin::default()
        .descriptor()
        .build_gts_manifest(&config_of(config))
        .expect("manifest")
}

/// What the runtime does before `init`: publish the manifest for `config`
/// through the gear's own publisher adapter, against `registry`.
async fn published(
    registry: Option<&Arc<MockTypesRegistry>>,
    config: &Value,
    cancel: &CancellationToken,
) -> Result<(), GtsPublicationError> {
    let hub = ClientHub::new();
    if let Some(registry) = registry {
        registry.install(&hub);
    }
    PreparedGtsPublication::prepare(
        StaticAuthNPlugin::default().descriptor(),
        &config_of(config),
        &hub,
    )?
    .publish(cancel, Arc::new(|_: &str| {}))
    .await
}

/// A context for `init`, carrying `admitted` as the runtime would.
fn ctx(config: &Value, admitted: Option<GtsManifest>) -> GearCtx {
    let ctx = GearCtx::new(
        "static-authn-plugin",
        uuid::Uuid::new_v4(),
        Arc::new(config_of(config)),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    );
    match admitted {
        Some(manifest) => ctx.with_gts_publication_manifest_for_test(Arc::new(manifest)),
        None => ctx,
    }
}

/// Selection among this process's `AuthN` plugins over `listed`, vendor `cf`.
fn choose(
    ctx: &GearCtx,
    listed: &[(String, Value)],
) -> Result<String, toolkit::plugins::ChoosePluginError> {
    toolkit::plugins::choose_registered_plugin_instance::<
        AuthNResolverPluginSpecV1,
        dyn AuthNResolverPluginClient,
    >(
        &ctx.client_hub(),
        "cf",
        listed
            .iter()
            .map(|(id, content)| (id.as_str(), Some(content))),
    )
}

/// The manifest is config-only: the configured instance, no `init` needed.
#[test]
fn the_manifest_is_the_configured_instance() {
    let manifest = manifest(&json!({ "vendor": "cf", "priority": 7 }));

    let documents = manifest.documents();
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].0.id(), instance_id());
    assert_eq!(documents[0].1["vendor"], "cf");
    assert_eq!(documents[0].1["priority"], 7);
}

/// `init` after admission binds the scoped client to the admitted instance;
/// the first selection settles on it.
#[tokio::test]
async fn init_binds_the_admitted_instance() {
    let registry = Arc::new(MockTypesRegistry::new());
    let config = json!({ "vendor": "cf", "priority": 7 });
    published(Some(&registry), &config, &CancellationToken::new())
        .await
        .expect("admitted");
    let ctx = ctx(&config, Some(manifest(&config)));

    StaticAuthNPlugin::default().init(&ctx).await.expect("init");

    assert!(bound(&ctx.client_hub()));
    let content = registry.content(&instance_id()).expect("admitted");
    assert_eq!(content["vendor"], "cf");
    assert_eq!(content["priority"], 7);
    assert_eq!(
        choose(&ctx, &[(instance_id(), content)]).expect("bound and settled"),
        instance_id()
    );
}

/// Without an admitted manifest — outside the runtime — `init` fails and
/// binds nothing; it never publishes.
#[tokio::test]
async fn init_without_an_admitted_manifest_binds_nothing() {
    let ctx = ctx(&json!({}), None);

    let err = StaticAuthNPlugin::default().init(&ctx).await.unwrap_err();

    assert!(
        format!("{err:#}").contains("admitted GTS manifest"),
        "{err:#}"
    );
    assert!(!bound(&ctx.client_hub()));
}

/// A repeated `init` is refused.
#[tokio::test]
async fn a_second_init_is_refused() {
    let config = json!({ "vendor": "cf" });
    let gear = StaticAuthNPlugin::default();
    gear.init(&ctx(&config, Some(manifest(&config))))
        .await
        .expect("first init");

    let err = gear
        .init(&ctx(&config, Some(manifest(&config))))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("already initialized"), "{err}");
}

/// A later boot under another configuration updates the durable instance.
#[tokio::test]
async fn a_changed_configuration_updates_the_instance() {
    let registry = Arc::new(MockTypesRegistry::new());
    let (_, previous) =
        PluginV1::<AuthNResolverPluginSpecV1>::build_registration(INSTANCE, "old", 1).unwrap();
    registry.seed(&instance_id(), previous);

    published(
        Some(&registry),
        &json!({ "vendor": "cf", "priority": 7 }),
        &CancellationToken::new(),
    )
    .await
    .expect("admitted");

    let content = registry.content(&instance_id()).expect("stored");
    assert_eq!(
        (&content["vendor"], &content["priority"]),
        (&json!("cf"), &json!(7))
    );
}

/// A terminal refusal fails the publication: the runtime never calls `init`.
#[tokio::test]
async fn a_refused_instance_fails_the_publication() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.reject(
        &instance_id(),
        AdmissionFailureReason::IncompatibleWithBaseline,
    );

    let err = published(Some(&registry), &json!({}), &CancellationToken::new())
        .await
        .unwrap_err();

    assert!(matches!(err, GtsPublicationError::Refused { .. }), "{err}");
    assert!(err.to_string().contains("publishing instance"), "{err}");
}

/// An accepted admission that never completes is polled within the publication
/// budget, submitted once; cancellation before the deadline ends it as cancelled.
/// (Pending progress is reported between reconciliation passes, so a single
/// polled operation reports none.)
#[tokio::test(start_paused = true)]
async fn a_pending_instance_is_polled_within_the_budget_until_cancelled() {
    let registry = Arc::new(MockTypesRegistry::new().completing_after(u32::MAX));
    let hub = ClientHub::new();
    registry.install(&hub);
    let publication = PreparedGtsPublication::prepare(
        StaticAuthNPlugin::default().descriptor(),
        &config_of(&json!({})),
        &hub,
    )
    .expect("prepared");
    let cancel = CancellationToken::new();

    let running = publication.publish(&cancel, Arc::new(|_: &str| {}));
    tokio::pin!(running);
    tokio::select! {
        result = &mut running => panic!("settled within the budget: {result:?}"),
        () = tokio::time::sleep(types_registry_sdk::STARTUP_PUBLICATION_DEADLINE / 2) => {}
    }

    assert_eq!(registry.calls(Call::Register), 1, "submitted once");
    assert!(registry.calls(Call::GetOperation) > 1, "polled");
    cancel.cancel();
    assert!(matches!(
        running.await,
        Err(GtsPublicationError::Cancelled { .. })
    ));
}

/// A publication still pending at budget expiry reports the deadline stop.
#[tokio::test(start_paused = true)]
async fn a_pending_publication_expires_once_the_budget_runs_out() {
    let registry = Arc::new(MockTypesRegistry::new().completing_after(u32::MAX));

    let result = published(Some(&registry), &json!({}), &CancellationToken::new()).await;

    let Err(GtsPublicationError::Expired { source, .. }) = result else {
        panic!("expected publication expiry: {result:?}");
    };
    let stop = source
        .chain()
        .find_map(|e| e.downcast_ref::<types_registry_sdk::PublicationError>())
        .and_then(types_registry_sdk::PublicationError::report)
        .map(|report| report.stop);
    assert_eq!(
        stop,
        Some(types_registry_sdk::ReconcileStop::Deadline),
        "{source:#}"
    );
}

/// No registry API in the hub: the publication cannot bind its backend.
#[tokio::test]
async fn without_the_platform_api_the_publication_fails() {
    let err = published(None, &json!({}), &CancellationToken::new())
        .await
        .unwrap_err();

    assert!(matches!(err, GtsPublicationError::Backend { .. }), "{err}");
}
