use std::error::Error as _;
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;

use crate::testing_platform::{Call, Fault, MockTypesRegistry};
use crate::{
    AdmissionFailureReason, GtsId, JsonDocument, PublicationRetry, PublisherContext,
    ReconcileOutcome, ReconcilePendingCause, ReconcileStop, Reconciliation,
    publish_startup_entities, publish_until_admitted,
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

// ---- publish_until_admitted --------------------------------------------------------------

fn is_cancelled(error: &crate::PublicationError) -> bool {
    matches!(error.canonical_error(), CanonicalError::Cancelled { .. }) && !error.is_transient()
}

/// A missing dependency stays pending, so the call retries until another writer stores it.
#[tokio::test(start_paused = true)]
async fn a_pending_dependency_is_retried_until_admitted() {
    let registry = MockTypesRegistry::new();
    registry.depends_on(A, B);
    let mut retries = 0;

    let result = publish_until_admitted(
        &registry,
        &publisher(),
        &[document(A)],
        &PublicationRetry::default(),
        &CancellationToken::new(),
        |error| {
            assert!(error.is_transient(), "{error}");
            retries += 1;
            if retries == 2 {
                registry.seed(B, json!({"title": B}));
            }
        },
    )
    .await
    .expect("admitted once the dependency exists");

    assert_eq!(retries, 2);
    assert!(matches!(result, Reconciliation::Reconciled(_)));
    assert_eq!(registry.content(A), Some(document(A).1));
}

/// A persistent outage retries until the caller cancels; then it reports caller
/// cancellation, not the outage, and makes no further call.
#[tokio::test(start_paused = true)]
async fn a_persistent_outage_retries_until_cancelled() {
    let registry = MockTypesRegistry::new();
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    let cancel = CancellationToken::new();
    let mut retries = 0;
    let mut reads_at_cancel = 0;

    let error = publish_until_admitted(
        &registry,
        &publisher(),
        &[document(A)],
        &PublicationRetry::default(),
        &cancel,
        |error| {
            assert!(error.is_transient(), "{error}");
            retries += 1;
            if retries == 3 {
                reads_at_cancel = registry.calls(Call::BatchGet);
                cancel.cancel();
            }
        },
    )
    .await
    .expect_err("cancelled");

    assert!(is_cancelled(&error), "{error}");
    assert_eq!(registry.calls(Call::BatchGet), reads_at_cancel);
    assert!(error.to_string().contains(A), "{error}");
}

/// An outage outlasting one attempt, then over: the next attempt publishes.
#[tokio::test(start_paused = true)]
async fn an_outage_outlasting_an_attempt_recovers() {
    let unavailable =
        || Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create());
    // Reads one attempt makes against a registry that never answers.
    let probe = MockTypesRegistry::new();
    probe.inject(unavailable());
    publish_startup_entities(
        &probe,
        &publisher(),
        &[document(A)],
        &CancellationToken::new(),
    )
    .await
    .expect_err("unavailable");
    let reads_per_attempt = probe.calls(Call::BatchGet);

    let registry = MockTypesRegistry::new();
    registry.inject(unavailable().first(reads_per_attempt));
    let mut retries = 0;

    publish_until_admitted(
        &registry,
        &publisher(),
        &[document(A)],
        &PublicationRetry::default(),
        &CancellationToken::new(),
        |_| retries += 1,
    )
    .await
    .expect("admitted after the outage");

    assert_eq!(retries, 1);
    assert_eq!(registry.content(A), Some(document(A).1));
}

/// A rejection is terminal: one batch admitting A and rejecting B is not retried into a
/// success, and nothing is reported as admitted.
#[tokio::test(start_paused = true)]
async fn a_partly_rejected_batch_is_terminal() {
    let registry = MockTypesRegistry::new();
    registry.reject(B, AdmissionFailureReason::InvalidSchema);

    let error = publish_until_admitted(
        &registry,
        &publisher(),
        &[document(A), document(B)],
        &PublicationRetry::default(),
        &CancellationToken::new(),
        |error| panic!("a rejection is not retried: {error}"),
    )
    .await
    .expect_err("B rejected");

    assert!(!error.is_transient());
    let report = error.report().expect("report");
    assert!(matches!(
        report.outcomes[&document(A).0],
        ReconcileOutcome::Admitted
    ));
    assert!(matches!(
        report.outcomes[&document(B).0],
        ReconcileOutcome::Rejected(_)
    ));
}

/// Admission is sticky: once A is admitted and B still pending, later attempts submit
/// only B — A changed meanwhile by another writer is left alone — and their errors
/// still cover A. A rejection of B after that is terminal and keeps A admitted.
#[tokio::test(start_paused = true)]
async fn admitted_identifiers_are_not_submitted_again() {
    let registry = MockTypesRegistry::new();
    registry.depends_on(B, C);
    let changed = json!({"title": "changed by another writer"});
    let mut retries = 0;

    let error = publish_until_admitted(
        &registry,
        &publisher(),
        &[document(A), document(B)],
        &PublicationRetry::default(),
        &CancellationToken::new(),
        |error| {
            retries += 1;
            let report = error.report().expect("report");
            assert!(matches!(
                report.outcomes[&document(A).0],
                ReconcileOutcome::Admitted
            ));
            assert!(matches!(
                report.outcomes[&document(B).0],
                ReconcileOutcome::Pending(_)
            ));
            if retries == 1 {
                registry.seed(A, changed.clone());
                registry.reject(B, AdmissionFailureReason::InvalidSchema);
                registry.seed(C, json!({"title": C}));
            }
        },
    )
    .await
    .expect_err("B rejected");

    assert_eq!(retries, 1);
    assert_eq!(
        registry.content(A),
        Some(changed),
        "A is not submitted again"
    );
    let report = error.report().expect("report");
    assert!(matches!(
        report.outcomes[&document(A).0],
        ReconcileOutcome::Admitted
    ));
    assert!(matches!(
        report.outcomes[&document(B).0],
        ReconcileOutcome::Rejected(_)
    ));
    assert!(!error.is_transient());
    // The canonical error, as a caller or the source chain decodes it, says the same.
    let decoded = crate::ReconcileReport::from_canonical(error.canonical_error()).expect("decodes");
    assert_eq!(decoded.stop, report.stop);
    let mut ids: Vec<_> = decoded
        .outcomes
        .keys()
        .map(|id| id.id().to_owned())
        .collect();
    ids.sort();
    assert_eq!(ids, [A, B]);
    assert!(matches!(
        decoded.outcomes[&document(A).0],
        ReconcileOutcome::Admitted
    ));
    assert!(matches!(
        decoded.outcomes[&document(B).0],
        ReconcileOutcome::Rejected(_)
    ));
}

/// A call-level refusal (here authentication) needs a fix elsewhere: not retried blindly.
#[tokio::test(start_paused = true)]
async fn a_refused_call_is_terminal() {
    for refusal in [
        CanonicalError::unauthenticated()
            .with_reason("TOKEN_INVALID")
            .create(),
        crate::gts::TypeResource::permission_denied()
            .with_reason("DENIED")
            .create(),
    ] {
        let registry = MockTypesRegistry::new();
        registry.inject(Fault::on(Call::Register).fail(refusal.clone()));

        let error = publish_until_admitted(
            &registry,
            &publisher(),
            &[document(A)],
            &PublicationRetry::default(),
            &CancellationToken::new(),
            |error| panic!("a refusal is not retried: {error}"),
        )
        .await
        .expect_err("refused");

        assert!(!error.is_transient(), "{refusal:?}");
        assert!(matches!(
            error.report().expect("report").outcomes[&document(A).0],
            ReconcileOutcome::Pending(ReconcilePendingCause::Refused(_))
        ));
    }
}

#[tokio::test]
async fn an_already_cancelled_call_reports_cancellation() {
    let registry = MockTypesRegistry::new();
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = publish_until_admitted(
        &registry,
        &publisher(),
        &[document(A)],
        &PublicationRetry::default(),
        &cancel,
        |error| panic!("cancellation is not retried: {error}"),
    )
    .await
    .expect_err("cancelled");

    assert!(is_cancelled(&error), "{error}");
    assert_eq!(registry.calls(Call::Register), 0);
}

#[test]
fn pauses_double_up_to_the_maximum() {
    let retry = PublicationRetry {
        initial: Duration::from_secs(1),
        max: Duration::from_secs(3),
    };
    let pauses: Vec<_> = retry.pauses().take(5).collect();
    assert_eq!(pauses, [1, 2, 3, 3, 3].map(Duration::from_secs));

    let capped = PublicationRetry {
        initial: Duration::from_secs(10),
        max: Duration::from_secs(3),
    };
    assert_eq!(capped.pauses().next(), Some(Duration::from_secs(3)));
}
