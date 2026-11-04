//! GTS declarations are published and admitted before the application can init.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use admission_control::AdmissionControl;
use admission_control_sdk::{ADMISSION_CONTROL_RESOURCE, AUDIT_TOPIC_ID, REFUSAL_EVENT_TYPE};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::Gear;
use toolkit::gts_publication::{
    GtsPublicationCapability as _, GtsPublicationError, PreparedGtsPublication,
};
use types_registry_sdk::AdmissionFailureReason;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};

fn provider() -> common::MockConfigProvider {
    common::MockConfigProvider::new(&json!({}))
}

#[test]
fn manifest_contains_exactly_the_four_owned_sdk_declarations() {
    let manifest = AdmissionControl::default()
        .descriptor()
        .build_gts_manifest(&provider())
        .unwrap();
    let expected: Vec<(String, serde_json::Value)> = admission_control_sdk::gts_declarations()
        .into_iter()
        .map(|(id, text)| (id, serde_json::from_str(&text).unwrap()))
        .collect();
    assert_eq!(manifest.documents(), expected);
    assert_eq!(manifest.documents().len(), 4);
    for id in [
        ADMISSION_CONTROL_RESOURCE,
        AUDIT_TOPIC_ID,
        REFUSAL_EVENT_TYPE,
    ] {
        assert!(
            manifest
                .documents()
                .iter()
                .any(|(declared, _)| declared == id),
            "missing {id}"
        );
    }
}

#[tokio::test]
async fn init_after_publication_registers_the_client_without_publishing_again() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = Arc::new(toolkit::ClientHub::new());
    registry.install(&hub);
    let manifest = Arc::new(
        AdmissionControl::default()
            .descriptor()
            .build_gts_manifest(&provider())
            .unwrap(),
    );
    let publication = PreparedGtsPublication::bind(
        AdmissionControl::default().descriptor(),
        Arc::clone(&manifest),
        &hub,
    )
    .unwrap();
    publication
        .publish(&CancellationToken::new(), Arc::new(|_: &str| {}))
        .await
        .unwrap();
    let registered = registry.calls(Call::Register);
    assert!(registered > 0);
    let ctx = toolkit::GearCtx::new(
        common::GEAR,
        uuid::Uuid::new_v4(),
        Arc::new(provider()),
        Arc::clone(&hub),
        CancellationToken::new(),
    )
    .with_gts_publication_manifest(manifest);
    AdmissionControl::default().init(&ctx).await.unwrap();
    assert!(
        hub.try_get::<dyn admission_control_sdk::AdmissionClientV1>()
            .is_some()
    );
    assert_eq!(registry.calls(Call::Register), registered);
    assert_eq!(
        registry.content(REFUSAL_EVENT_TYPE).unwrap(),
        admission_control_sdk::gts::refusal_event_type_schema()
    );
}

#[tokio::test]
async fn rejected_refusal_schema_prevents_publication_completion() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.reject(
        REFUSAL_EVENT_TYPE,
        AdmissionFailureReason::IncompatibleWithBaseline,
    );
    let hub = toolkit::ClientHub::new();
    registry.install(&hub);
    let err = PreparedGtsPublication::prepare(
        AdmissionControl::default().descriptor(),
        &provider(),
        &hub,
    )
    .unwrap()
    .publish(&CancellationToken::new(), Arc::new(|_: &str| {}))
    .await
    .unwrap_err();
    assert!(matches!(err, GtsPublicationError::Refused { .. }), "{err}");
    assert!(
        hub.try_get::<dyn admission_control_sdk::AdmissionClientV1>()
            .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn an_unreachable_registry_reaches_the_publication_deadline() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.inject(
        Fault::on(Call::Register)
            .fail(toolkit_canonical_errors::CanonicalError::service_unavailable().create()),
    );
    let hub = toolkit::ClientHub::new();
    registry.install(&hub);
    let publication = PreparedGtsPublication::prepare(
        AdmissionControl::default().descriptor(),
        &provider(),
        &hub,
    )
    .unwrap();
    let started = tokio::time::Instant::now();

    let result = publication
        .publish(&CancellationToken::new(), Arc::new(|_: &str| {}))
        .await;

    assert_eq!(
        started.elapsed(),
        types_registry_sdk::STARTUP_PUBLICATION_DEADLINE
    );
    let Err(GtsPublicationError::Refused { source, .. }) = result else {
        panic!("expected publication deadline: {result:?}");
    };
    let report = source
        .chain()
        .find_map(|error| error.downcast_ref::<types_registry_sdk::PublicationError>())
        .and_then(types_registry_sdk::PublicationError::report)
        .expect("reconciliation report");
    assert_eq!(
        report.stop,
        types_registry_sdk::ReconcileStop::Deadline,
        "{source:#}"
    );
    assert_eq!(
        report.outcomes.len(),
        publication.gts_manifest().documents().len()
    );
    assert!(report.outcomes.values().all(|outcome| matches!(
        outcome,
        types_registry_sdk::ReconcileOutcome::Pending(
            types_registry_sdk::ReconcilePendingCause::Unavailable(_)
        )
    )));
    assert!(registry.calls(Call::Register) > 1, "publication retried");
    assert!(
        hub.try_get::<dyn admission_control_sdk::AdmissionClientV1>()
            .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn an_unreachable_registry_can_be_cancelled_before_the_deadline() {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.inject(
        Fault::on(Call::Register)
            .fail(toolkit_canonical_errors::CanonicalError::service_unavailable().create()),
    );
    let hub = toolkit::ClientHub::new();
    registry.install(&hub);
    let publication = PreparedGtsPublication::prepare(
        AdmissionControl::default().descriptor(),
        &provider(),
        &hub,
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let publishing = publication.publish(&cancel, Arc::new(|_: &str| {}));
    tokio::pin!(publishing);

    tokio::select! {
        result = &mut publishing => panic!("settled before cancellation: {result:?}"),
        () = tokio::time::sleep(types_registry_sdk::STARTUP_PUBLICATION_DEADLINE / 2) => {}
    }
    let submissions = registry.calls(Call::Register);
    assert!(submissions > 1, "publication retried");
    assert!(
        hub.try_get::<dyn admission_control_sdk::AdmissionClientV1>()
            .is_none()
    );

    cancel.cancel();
    assert!(matches!(
        publishing.await,
        Err(GtsPublicationError::Cancelled { .. })
    ));
    assert_eq!(
        registry.calls(Call::Register),
        submissions,
        "no submissions after cancellation"
    );
    assert!(
        hub.try_get::<dyn admission_control_sdk::AdmissionClientV1>()
            .is_none()
    );
}
