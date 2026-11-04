use std::error::Error as _;
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;

use crate::testing_platform::{Call, Fault, MockTypesRegistry};
use crate::{
    AdmissionFailureReason, GtsId, JsonDocument, PublisherContext, ReconcileOutcome,
    ReconcilePendingCause, ReconcileStop, Reconciliation, publish_startup_entities,
};

const A: &str = "gts.cf.test.publication.a.v1~";
const B: &str = "gts.cf.test.publication.b.v1~";
const C: &str = "gts.cf.test.publication.c.v1~";

fn publisher() -> PublisherContext {
    PublisherContext {
        name: "publication-test".to_owned(),
        version: "3.2.1".parse().expect("version"),
    }
}

fn document(id: &str) -> (GtsId, JsonDocument) {
    (
        GtsId::try_new(id).expect("identifier"),
        json!({"title": id}),
    )
}

#[tokio::test]
async fn publishes_a_batch_with_the_callers_identity() {
    let registry = MockTypesRegistry::new();
    let desired = [document(A), document(B)];
    let publisher = publisher();

    let result =
        publish_startup_entities(&registry, &publisher, &desired, &CancellationToken::new())
            .await
            .expect("both documents admitted");

    let Reconciliation::Reconciled(outcomes) = result else {
        panic!("absent documents need publication");
    };
    for (id, content) in &desired {
        assert!(matches!(outcomes[id], ReconcileOutcome::Admitted));
        assert_eq!(registry.content(id.id()), Some(content.clone()));
    }
    assert!(
        registry
            .submissions()
            .iter()
            .all(|(_, request)| request.publisher == publisher)
    );
}

#[tokio::test]
async fn an_equal_restart_submits_nothing() {
    let registry = MockTypesRegistry::new();
    let desired = [document(A)];
    registry.seed(A, desired[0].1.clone());

    let result =
        publish_startup_entities(&registry, &publisher(), &desired, &CancellationToken::new())
            .await
            .expect("already published");

    assert!(matches!(result, Reconciliation::UpToDate));
    assert_eq!(registry.calls(Call::Register), 0);
}

#[tokio::test]
async fn a_batch_failure_names_every_rejected_id_and_preserves_its_report() {
    let registry = MockTypesRegistry::new();
    registry.reject(A, AdmissionFailureReason::InvalidSchema);
    registry.reject(B, AdmissionFailureReason::InvalidValue);
    let desired = [document(B), document(C), document(A)];

    let error =
        publish_startup_entities(&registry, &publisher(), &desired, &CancellationToken::new())
            .await
            .expect_err("two documents refused");

    let message = error.to_string();
    for fragment in [
        "publication-test",
        "completed",
        A,
        B,
        "invalid_schema",
        "invalid_value",
    ] {
        assert!(message.contains(fragment), "{message}");
    }
    assert!(
        !message.contains(C),
        "admitted IDs need no failure diagnostic: {message}"
    );
    assert!(
        message.find(A) < message.find(B),
        "IDs have deterministic ordering: {message}"
    );
    let report = error.report().expect("structured report");
    assert_eq!(report.stop, ReconcileStop::Completed);
    assert_eq!(report.outcomes.len(), 3);
    assert!(matches!(
        report.outcomes[&desired[1].0],
        ReconcileOutcome::Admitted
    ));
    let source = error.source().expect("canonical source");
    assert!(source.downcast_ref::<CanonicalError>().is_some());
    let wrapped = anyhow::Error::new(error);
    assert!(
        wrapped
            .chain()
            .any(|cause| cause.downcast_ref::<CanonicalError>().is_some())
    );
}

#[tokio::test(start_paused = true)]
async fn pending_dependencies_are_not_success() {
    let registry = MockTypesRegistry::new();
    registry.depends_on(A, B);
    let desired = [document(A)];

    let error =
        publish_startup_entities(&registry, &publisher(), &desired, &CancellationToken::new())
            .await
            .expect_err("a dependency is still missing");

    let report = error.report().expect("pending report");
    assert!(matches!(
        report.outcomes[&desired[0].0],
        ReconcileOutcome::Pending(ReconcilePendingCause::Dependency(_))
    ));
    let message = error.to_string();
    for fragment in [A, "pending", "dependency"] {
        assert!(message.contains(fragment), "{message}");
    }
}

#[tokio::test(start_paused = true)]
async fn startup_publication_allows_more_than_the_default_sixty_seconds() {
    let registry = MockTypesRegistry::new();
    registry.inject(
        Fault::on(Call::BatchGet)
            .first(1)
            .delay(Duration::from_secs(90)),
    );

    publish_startup_entities(
        &registry,
        &publisher(),
        &[document(A)],
        &CancellationToken::new(),
    )
    .await
    .expect("startup has a 120-second budget");

    assert_eq!(registry.content(A), Some(document(A).1));
}

#[tokio::test]
async fn cancellation_preserves_the_canonical_error_and_names_requested_ids() {
    let registry = MockTypesRegistry::new();
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = publish_startup_entities(
        &registry,
        &publisher(),
        &[document(B), document(A)],
        &cancel,
    )
    .await
    .expect_err("cancelled");

    assert!(matches!(
        error.canonical_error(),
        CanonicalError::Cancelled { .. }
    ));
    assert!(error.report().is_none());
    let message = error.to_string();
    for fragment in ["publication-test", "cancelled", A, B] {
        assert!(message.contains(fragment), "{message}");
    }
    assert_eq!(registry.calls(Call::Register), 0);
}

#[tokio::test]
async fn an_unavailable_registry_keeps_the_pending_cause() {
    let registry = MockTypesRegistry::new();
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    let desired = [document(A)];

    let error =
        publish_startup_entities(&registry, &publisher(), &desired, &CancellationToken::new())
            .await
            .expect_err("unavailable");

    let report = error.report().expect("read-failure report");
    assert_eq!(report.stop, ReconcileStop::ReadFailed);
    assert!(matches!(
        report.outcomes[&desired[0].0],
        ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(_))
    ));
    let message = error.to_string();
    assert!(
        message.contains(A) && message.contains("service_unavailable"),
        "{message}"
    );
    assert_eq!(registry.calls(Call::Register), 0);
}
