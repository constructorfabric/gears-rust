//! The runtime publishes the SDK catalog and configured instance before init.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use policy_engine::PolicyEngine;
use policy_engine::domain::engine_plugin;
use policy_engine_sdk::gts::permissions::Capability;
use policy_engine_sdk::{
    ASSIGNMENT_RESOURCE, BUNDLE_RESOURCE, BUNDLE_VERSION_RESOURCE, PolicyManagementClientV1,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::Gear;
use toolkit::gts_publication::{
    GtsPublicationCapability as _, GtsPublicationError, PreparedGtsPublication,
};
use types_registry_sdk::AdmissionFailureReason;
use types_registry_sdk::testing_platform::{Call, Fault};

fn config() -> Value {
    json!({ "engine_plugin": { "vendor": "constructorfabric", "priority": 100 } })
}

#[tokio::test]
async fn the_owned_resource_types_permissions_and_configured_instance_are_published_before_init() {
    let registry = common::registry();
    let hub = Arc::new(toolkit::ClientHub::new());
    registry.install(&hub);
    let config = config();
    let provider = common::MockConfigProvider::new(&config);
    let manifest = PolicyEngine::default()
        .descriptor()
        .build_gts_manifest(&provider)
        .unwrap();
    assert_eq!(manifest.documents().len(), 7);
    PreparedGtsPublication::prepare(PolicyEngine::default().descriptor(), &provider, &hub)
        .unwrap()
        .publish(&CancellationToken::new(), Arc::new(|_: &str| {}))
        .await
        .unwrap();
    for id in [
        BUNDLE_RESOURCE,
        BUNDLE_VERSION_RESOURCE,
        ASSIGNMENT_RESOURCE,
    ] {
        assert!(registry.content(id).is_some(), "missing resource {id}");
    }
    for capability in Capability::ALL {
        let content = registry.content(capability.permission_id()).unwrap();
        assert_eq!(content["resource_type"], BUNDLE_RESOURCE);
        assert_eq!(content["action"], capability.action());
    }
    let (instance, expected) = engine_plugin::registration("constructorfabric", 100).unwrap();
    assert_eq!(registry.content(instance.as_ref()).unwrap(), expected);
    let writes = registry.calls(Call::Register);
    let ctx = common::gear_ctx_over(&config, Arc::clone(&hub)).await;
    PolicyEngine::default().init(&ctx).await.unwrap();
    assert_eq!(
        registry.calls(Call::Register),
        writes,
        "init binds without republishing"
    );
    assert!(
        hub.try_get_local::<dyn PolicyManagementClientV1>()
            .is_some()
    );
}

#[tokio::test]
async fn a_refused_plugin_instance_prevents_publication_completion_and_binding() {
    let registry = common::registry();
    let (id, _) = engine_plugin::registration("constructorfabric", 100).unwrap();
    registry.reject(id.as_ref(), AdmissionFailureReason::InvalidSchema);
    let hub = toolkit::ClientHub::new();
    registry.install(&hub);
    let error = PreparedGtsPublication::prepare(
        PolicyEngine::default().descriptor(),
        &common::MockConfigProvider::new(&config()),
        &hub,
    )
    .unwrap()
    .publish(&CancellationToken::new(), Arc::new(|_: &str| {}))
    .await
    .unwrap_err();
    assert!(
        matches!(error, GtsPublicationError::Refused { .. }),
        "{error}"
    );
    assert!(
        hub.try_get_local::<dyn PolicyManagementClientV1>()
            .is_none()
    );
    assert!(
        hub.try_get_scoped::<dyn admission_control_sdk::AdmissionEnginePluginClientV1>(
            &toolkit::client_hub::ClientScope::gts_id(&id),
        )
        .is_none()
    );
}

/// An unreachable registry is retried within the publication budget;
/// cancellation before the deadline ends the publication as cancelled.
#[tokio::test(start_paused = true)]
async fn an_unreachable_registry_is_retried_within_the_budget_until_cancelled() {
    let registry = common::registry();
    registry.inject(
        Fault::on(Call::Register)
            .fail(toolkit_canonical_errors::CanonicalError::service_unavailable().create()),
    );
    let hub = toolkit::ClientHub::new();
    registry.install(&hub);
    let publication = PreparedGtsPublication::prepare(
        PolicyEngine::default().descriptor(),
        &common::MockConfigProvider::new(&config()),
        &hub,
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let publishing = publication.publish(&cancel, Arc::new(|_: &str| {}));
    tokio::pin!(publishing);
    tokio::select! {
        result = &mut publishing => panic!("settled within the budget: {result:?}"),
        () = tokio::time::sleep(types_registry_sdk::STARTUP_PUBLICATION_DEADLINE / 2) => {}
    }
    assert!(registry.calls(Call::Register) > 1);
    cancel.cancel();
    assert!(matches!(
        publishing.await,
        Err(GtsPublicationError::Cancelled { .. })
    ));
}

/// Publication against an unreachable registry expires with the deadline stop.
#[tokio::test(start_paused = true)]
async fn an_unreachable_registry_exhausts_the_publication_budget() {
    let registry = common::registry();
    registry.inject(
        Fault::on(Call::Register)
            .fail(toolkit_canonical_errors::CanonicalError::service_unavailable().create()),
    );
    let hub = toolkit::ClientHub::new();
    registry.install(&hub);
    let publication = PreparedGtsPublication::prepare(
        PolicyEngine::default().descriptor(),
        &common::MockConfigProvider::new(&config()),
        &hub,
    )
    .unwrap();

    let result = publication
        .publish(&CancellationToken::new(), Arc::new(|_: &str| {}))
        .await;

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
