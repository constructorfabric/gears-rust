use std::error::Error as _;
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;

use super::{GtsPublishError, publish, publish_with_deadline};
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

#[tokio::test(start_paused = true)]
async fn an_unavailable_registry_keeps_the_pending_cause() {
    let registry = MockTypesRegistry::new();
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    let desired = [document(A)];

    let error =
        publish_startup_entities(&registry, &publisher(), &desired, &CancellationToken::new())
            .await
            .expect_err("unavailable");

    let report = error.report().expect("read-failure report");
    assert_eq!(report.stop, ReconcileStop::Deadline);
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

#[tokio::test(start_paused = true)]
async fn the_callers_budget_can_exceed_the_startup_default() {
    let registry = MockTypesRegistry::new();
    registry.inject(
        Fault::on(Call::BatchGet)
            .first(1)
            .delay(Duration::from_secs(150)),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);

    publish_with_deadline(
        &registry,
        "my-gear",
        "1.0.0",
        &[(A.to_owned(), json!({}))],
        &CancellationToken::new(),
        Some(deadline),
        |_| {},
    )
    .await
    .expect("the caller grants more than the default 120 seconds");

    assert_eq!(registry.content(A), Some(json!({})));
    assert!(tokio::time::Instant::now() < deadline);
}

#[tokio::test(start_paused = true)]
async fn the_callers_deadline_bounds_all_retries() {
    let registry = MockTypesRegistry::new();
    registry.inject(
        Fault::on(Call::BatchGet)
            .fail(toolkit_canonical_errors::CanonicalError::service_unavailable().create()),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

    let error = publish_with_deadline(
        &registry,
        "my-gear",
        "1.0.0",
        &[(A.to_owned(), json!({}))],
        &CancellationToken::new(),
        Some(deadline),
        |_| {},
    )
    .await
    .expect_err("the registry never recovers");

    assert_eq!(tokio::time::Instant::now(), deadline);
    let GtsPublishError::Publication(error) = error else {
        panic!("{error}")
    };
    assert_eq!(
        error.report().expect("report").stop,
        crate::ReconcileStop::Deadline
    );
    assert!(registry.submissions().is_empty());
}

#[tokio::test]
async fn publishes_the_manifest_under_the_given_identity() {
    let registry = MockTypesRegistry::new();
    let documents = [(A.to_owned(), json!({"title": "a"}))];

    publish(
        &registry,
        "my-gear",
        "1.2.3",
        &documents,
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .expect("admitted");

    assert_eq!(registry.content(A), Some(json!({"title": "a"})));
    let submissions = registry.submissions();
    assert!(!submissions.is_empty());
    for (_, request) in submissions {
        assert_eq!(request.publisher.name, "my-gear");
        assert_eq!(request.publisher.version.to_string(), "1.2.3");
    }
}

#[tokio::test]
async fn a_rejection_is_a_terminal_publication_error() {
    let registry = MockTypesRegistry::new();
    registry.reject(A, AdmissionFailureReason::InvalidSchema);

    let err = publish(
        &registry,
        "my-gear",
        "1.0.0",
        &[(A.to_owned(), json!({}))],
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();

    assert!(matches!(err, GtsPublishError::Publication(_)), "{err}");
    assert!(err.to_string().contains(A), "{err}");
}

#[tokio::test]
async fn invalid_input_is_reported_before_any_request() {
    let registry = MockTypesRegistry::new();
    for (version, id) in [("not-semver", A), ("1.0.0", "not a gts id")] {
        let err = publish(
            &registry,
            "my-gear",
            version,
            &[(id.to_owned(), json!({}))],
            &CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(err, GtsPublishError::Input(_)), "{err}");
    }
    assert!(registry.submissions().is_empty());
}

fn manifest(ids: &[&str]) -> Vec<(String, JsonDocument)> {
    ids.iter()
        .map(|id| ((*id).to_owned(), json!({"title": id})))
        .collect()
}

fn publication_error(error: GtsPublishError) -> crate::PublicationError {
    let GtsPublishError::Publication(error) = error else {
        panic!("{error}")
    };
    error
}

fn is_cancelled(error: &crate::PublicationError) -> bool {
    matches!(error.canonical_error(), CanonicalError::Cancelled { .. }) && !error.is_transient()
}

#[tokio::test(start_paused = true)]
async fn publication_retries_share_one_startup_budget() {
    let registry = MockTypesRegistry::new();
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    let started = tokio::time::Instant::now();
    let desired = manifest(&[A]);
    let cancel = CancellationToken::new();

    let result = tokio::time::timeout(
        crate::STARTUP_PUBLICATION_DEADLINE + Duration::from_secs(1),
        publish(&registry, "my-gear", "1.0.0", &desired, &cancel, |_| {}),
    )
    .await
    .expect("publication must finish within its budget");

    let error = result.expect_err("the registry never recovered");
    let error = publication_error(error);
    assert_eq!(started.elapsed(), crate::STARTUP_PUBLICATION_DEADLINE);
    assert_eq!(
        error.report().expect("report").stop,
        ReconcileStop::Deadline
    );
    assert!(registry.submissions().is_empty());
}

/// A missing dependency stays pending, so the call retries until another writer stores it.
#[tokio::test(start_paused = true)]
async fn a_pending_dependency_is_retried_until_admitted() {
    let registry = MockTypesRegistry::new();
    registry.depends_on(A, B);
    let mut retries = 0;

    publish(
        &registry,
        "my-gear",
        "1.0.0",
        &manifest(&[A]),
        &CancellationToken::new(),
        |detail| {
            assert!(detail.contains("pending.dependency"), "{detail}");
            assert!(detail.contains("retrying"), "{detail}");
            retries += 1;
            if retries == 2 {
                registry.seed(B, json!({"title": B}));
            }
        },
    )
    .await
    .expect("admitted once the dependency exists");

    assert_eq!(retries, 2);
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

    let error = publish(
        &registry,
        "my-gear",
        "1.0.0",
        &manifest(&[A]),
        &cancel,
        |detail| {
            assert!(detail.contains("pending.unavailable"), "{detail}");
            retries += 1;
            if retries == 3 {
                reads_at_cancel = registry.calls(Call::BatchGet);
                cancel.cancel();
            }
        },
    )
    .await
    .expect_err("cancelled");
    let error = publication_error(error);

    assert!(is_cancelled(&error), "{error}");
    assert_eq!(registry.calls(Call::BatchGet), reads_at_cancel);
    assert!(error.to_string().contains(A), "{error}");
}

/// Temporary read failures recover inside the same publication budget.
#[tokio::test(start_paused = true)]
async fn a_temporary_read_outage_recovers() {
    let registry = MockTypesRegistry::new();
    registry.inject(
        Fault::on(Call::BatchGet)
            .first(2)
            .fail(CanonicalError::service_unavailable().create()),
    );
    let mut retries = 0;

    publish(
        &registry,
        "my-gear",
        "1.0.0",
        &manifest(&[A]),
        &CancellationToken::new(),
        |_| retries += 1,
    )
    .await
    .expect("admitted after the outage");

    assert_eq!(retries, 2);
    assert_eq!(registry.content(A), Some(document(A).1));
}

/// A rejection is terminal: one batch admitting A and rejecting B is not retried into a
/// success, and nothing is reported as admitted.
#[tokio::test(start_paused = true)]
async fn a_partly_rejected_batch_is_terminal() {
    let registry = MockTypesRegistry::new();
    registry.reject(B, AdmissionFailureReason::InvalidSchema);

    let error = publish(
        &registry,
        "my-gear",
        "1.0.0",
        &manifest(&[A, B]),
        &CancellationToken::new(),
        |error| panic!("a rejection is not retried: {error}"),
    )
    .await
    .expect_err("B rejected");
    let error = publication_error(error);

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

    let error = publish(
        &registry,
        "my-gear",
        "1.0.0",
        &manifest(&[A, B]),
        &CancellationToken::new(),
        |detail| {
            retries += 1;
            assert!(detail.contains(B), "{detail}");
            assert!(detail.contains("pending.dependency"), "{detail}");
            assert!(
                !detail.contains(A),
                "an admitted ID needs no failure diagnostic: {detail}"
            );
            if retries == 1 {
                registry.seed(A, changed.clone());
                registry.reject(B, AdmissionFailureReason::InvalidSchema);
                registry.seed(C, json!({"title": C}));
            }
        },
    )
    .await
    .expect_err("B rejected");
    let error = publication_error(error);

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

        let error = publish(
            &registry,
            "my-gear",
            "1.0.0",
            &manifest(&[A]),
            &CancellationToken::new(),
            |error| panic!("a refusal is not retried: {error}"),
        )
        .await
        .expect_err("refused");
        let error = publication_error(error);

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

    let error = publish(
        &registry,
        "my-gear",
        "1.0.0",
        &manifest(&[A]),
        &cancel,
        |error| panic!("cancellation is not retried: {error}"),
    )
    .await
    .expect_err("cancelled");
    let error = publication_error(error);

    assert!(is_cancelled(&error), "{error}");
    assert_eq!(registry.calls(Call::Register), 0);
}

#[tokio::test(start_paused = true)]
async fn publication_backoff_doubles_with_jitter_up_to_thirty_seconds() {
    let registry = MockTypesRegistry::new().completing_after(0);
    registry.depends_on(A, B);
    let cancel = CancellationToken::new();
    let mut retries = Vec::new();
    let error = publish(
        &registry,
        "my-gear",
        "1.0.0",
        &manifest(&[A]),
        &cancel,
        |_| {
            retries.push(tokio::time::Instant::now());
            if retries.len() == 8 {
                cancel.cancel();
            }
        },
    )
    .await
    .expect_err("cancelled after observing the pauses");

    assert!(is_cancelled(&publication_error(error)));
    assert_eq!(retries.len(), 8);
    for (pair, seconds) in retries.windows(2).zip([1, 2, 4, 8, 16, 30, 30]) {
        let ceiling = Duration::from_secs(seconds);
        assert!((ceiling / 2..=ceiling).contains(&pair[1].duration_since(pair[0])));
    }
}
