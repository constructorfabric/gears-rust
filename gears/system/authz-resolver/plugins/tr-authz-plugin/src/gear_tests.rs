use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverPluginClient, AuthZResolverPluginSpecV1};
use serde_json::json;
use tenant_resolver_sdk::TenantResolverClient;
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

use super::TrAuthZPlugin;
use crate::domain::test_support::MockTr;

const SEGMENT: &str = "cf.builtin.tr_authz_resolver.plugin.v1";

struct Config(serde_json::Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.0.get(gear)
    }
}

fn ctx(hub: &Arc<ClientHub>, priority: i16, cancel: CancellationToken) -> GearCtx {
    GearCtx::new(
        TrAuthZPlugin::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(Config(json!({
            TrAuthZPlugin::MODULE_NAME: { "config": {
                "vendor": "acme",
                "priority": priority,
            } }
        }))),
        Arc::clone(hub),
        cancel,
    )
}

fn instance(priority: i16) -> (String, serde_json::Value) {
    let (id, content) =
        PluginV1::<AuthZResolverPluginSpecV1>::build_registration(SEGMENT, "acme", priority)
            .expect("plugin instance serializes");
    (id.as_ref().to_owned(), content)
}

/// A hub with the platform registry and a tenant resolver.
fn host() -> (Arc<ClientHub>, Arc<MockTypesRegistry>) {
    let hub = Arc::new(ClientHub::new());
    let registry = Arc::new(MockTypesRegistry::new());
    registry.install(&hub);
    hub.register::<dyn TenantResolverClient>(Arc::new(MockTr::empty()));
    (hub, registry)
}

fn backend_published(hub: &ClientHub, id: &str) -> bool {
    hub.try_get_scoped::<dyn AuthZResolverPluginClient>(&ClientScope::gts_id(id))
        .is_some()
}

/// The manifest the runtime publishes for `priority`, without `init`.
fn manifest(priority: i16) -> GtsManifest {
    TrAuthZPlugin::default()
        .descriptor()
        .build_gts_manifest(&Config(json!({
            TrAuthZPlugin::MODULE_NAME: { "config": { "vendor": "acme", "priority": priority } }
        })))
        .expect("manifest")
}

/// The runtime's publication of the manifest for `priority` under `cancel`.
async fn published(
    hub: &ClientHub,
    priority: i16,
    cancel: &CancellationToken,
) -> Result<(), GtsPublicationError> {
    PreparedGtsPublication::bind(
        TrAuthZPlugin::default().descriptor(),
        Arc::new(manifest(priority)),
        hub,
    )?
    .publish(cancel, Arc::new(|_: &str| {}))
    .await
}

/// `init` with the admitted manifest for `priority`, as the runtime calls it.
async fn admitted_init(hub: &Arc<ClientHub>, priority: i16) -> anyhow::Result<()> {
    TrAuthZPlugin::default()
        .init(
            &ctx(hub, priority, CancellationToken::new())
                .with_gts_publication_manifest(Arc::new(manifest(priority))),
        )
        .await
}

#[test]
fn the_manifest_is_the_configured_instance() {
    assert_eq!(manifest(7).documents(), [instance(7)]);
}

#[tokio::test]
async fn init_binds_the_admitted_instance() {
    let (hub, registry) = host();
    let (id, content) = instance(7);

    published(&hub, 7, &CancellationToken::new())
        .await
        .expect("admitted");
    assert!(!backend_published(&hub, &id), "nothing bound before init");
    admitted_init(&hub, 7).await.expect("init succeeds");

    assert_eq!(registry.content(&id), Some(content));
    assert!(backend_published(&hub, &id));
}

#[tokio::test]
async fn an_equal_restart_submits_nothing() {
    let (hub, registry) = host();
    let (id, content) = instance(7);
    registry.seed(&id, content);

    published(&hub, 7, &CancellationToken::new())
        .await
        .expect("admitted");

    assert_eq!(registry.calls(Call::Register), 0);
}

#[tokio::test]
async fn a_changed_priority_updates_the_stored_instance() {
    let (hub, registry) = host();
    let (id, before) = instance(7);
    registry.seed(&id, before);

    published(&hub, 3, &CancellationToken::new())
        .await
        .expect("admitted");

    assert_eq!(registry.content(&id), Some(instance(3).1));
}

/// A refusal fails the publication: the runtime never calls `init`.
#[tokio::test]
async fn a_rejected_instance_fails_the_publication() {
    let (hub, registry) = host();
    let (id, _) = instance(7);
    registry.reject(&id, AdmissionFailureReason::InvalidValue);

    let error = published(&hub, 7, &CancellationToken::new())
        .await
        .expect_err("a rejected instance");

    assert!(
        matches!(error, GtsPublicationError::Refused { .. }),
        "{error}"
    );
    assert!(error.to_string().contains(&id), "{error:#}");
}

/// A pending admission exhausts one publication budget without binding a client.
#[tokio::test(start_paused = true)]
async fn a_pending_instance_reaches_the_deadline_without_binding() {
    let hub = Arc::new(ClientHub::new());
    let registry = Arc::new(MockTypesRegistry::new().completing_after(u32::MAX));
    registry.install(&hub);
    let (id, _) = instance(7);
    let started = tokio::time::Instant::now();

    let result = published(&hub, 7, &CancellationToken::new()).await;

    assert_eq!(
        started.elapsed(),
        types_registry_sdk::STARTUP_PUBLICATION_DEADLINE
    );
    let Err(GtsPublicationError::Refused { source, .. }) = result else {
        panic!("expected publication deadline: {result:?}");
    };
    let stop = source
        .chain()
        .find_map(|error| error.downcast_ref::<types_registry_sdk::PublicationError>())
        .and_then(types_registry_sdk::PublicationError::report)
        .map(|report| report.stop);
    assert_eq!(
        stop,
        Some(types_registry_sdk::ReconcileStop::Deadline),
        "{source:#}"
    );
    assert_eq!(registry.calls(Call::Register), 1, "accepted once");
    assert!(
        registry.calls(Call::GetOperation) > 1,
        "polled the accepted operation"
    );
    assert!(!backend_published(&hub, &id));
}

/// Cancellation within the budget stops polling without binding the pending instance.
#[tokio::test(start_paused = true)]
async fn a_pending_instance_can_be_cancelled_before_the_deadline() {
    let hub = Arc::new(ClientHub::new());
    let registry = Arc::new(MockTypesRegistry::new().completing_after(u32::MAX));
    registry.install(&hub);
    let (id, _) = instance(7);
    let cancel = CancellationToken::new();
    let running = published(&hub, 7, &cancel);
    tokio::pin!(running);

    tokio::select! {
        result = &mut running => panic!("settled before cancellation: {result:?}"),
        () = tokio::time::sleep(types_registry_sdk::STARTUP_PUBLICATION_DEADLINE / 2) => {}
    }
    assert_eq!(registry.calls(Call::Register), 1, "accepted once");
    let polls = registry.calls(Call::GetOperation);
    assert!(polls > 1, "polled the accepted operation");
    assert!(!backend_published(&hub, &id));

    cancel.cancel();
    assert!(matches!(
        running.await,
        Err(GtsPublicationError::Cancelled { .. })
    ));
    assert_eq!(
        registry.calls(Call::GetOperation),
        polls,
        "no polls after cancellation"
    );
    assert!(!backend_published(&hub, &id));
}

#[tokio::test]
async fn a_host_without_the_platform_registry_api_cannot_publish() {
    let hub = Arc::new(ClientHub::new());

    let error = published(&hub, 7, &CancellationToken::new())
        .await
        .expect_err("no registry, no fallback");

    assert!(
        matches!(error, GtsPublicationError::Backend { .. }),
        "{error}"
    );
}

/// Without the tenant resolver the plugin could not evaluate anything: its
/// `init` fails before binding.
#[tokio::test]
async fn a_host_without_the_tenant_resolver_fails_init_before_binding() {
    let hub = Arc::new(ClientHub::new());
    let (id, _) = instance(7);

    admitted_init(&hub, 7)
        .await
        .expect_err("no tenant resolver");

    assert!(!backend_published(&hub, &id));
}

/// Outside the runtime there is no admitted manifest: nothing is bound.
#[tokio::test]
async fn init_without_an_admitted_manifest_binds_nothing() {
    let (hub, _registry) = host();
    let (id, _) = instance(7);

    let error = TrAuthZPlugin::default()
        .init(&ctx(&hub, 7, CancellationToken::new()))
        .await
        .expect_err("no admitted manifest");

    assert!(
        format!("{error:#}").contains("admitted GTS manifest"),
        "{error:#}"
    );
    assert!(!backend_published(&hub, &id));
}
