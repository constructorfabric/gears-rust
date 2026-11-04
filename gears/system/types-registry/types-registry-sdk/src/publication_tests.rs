use std::error::Error as _;
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;

use super::Publication;
use crate::testing_platform::{Call, Fault, MockTypesRegistry};
use crate::{
    AdmissionFailureReason, GtsId, JsonDocument, PublisherContext, ReconcileOutcome,
    ReconcilePendingCause, ReconcileStop, Reconciliation,
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

    let result = Publication::new(&registry, publisher.clone(), &desired)
        .publish(&CancellationToken::new())
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

    let result = Publication::new(&registry, publisher(), &desired)
        .publish(&CancellationToken::new())
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

    let error = Publication::new(&registry, publisher(), &desired)
        .publish(&CancellationToken::new())
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

    let error = Publication::new(&registry, publisher(), &desired)
        .publish(&CancellationToken::new())
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

    Publication::new(&registry, publisher(), [document(A)])
        .publish(&CancellationToken::new())
        .await
        .expect("startup has a 120-second budget");

    assert_eq!(registry.content(A), Some(document(A).1));
}

#[tokio::test]
async fn cancellation_preserves_the_canonical_error_and_names_requested_ids() {
    let registry = MockTypesRegistry::new();
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = Publication::new(&registry, publisher(), [document(B), document(A)])
        .publish(&cancel)
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

    let error = Publication::new(&registry, publisher(), &desired)
        .publish(&CancellationToken::new())
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
fn manifest(ids: &[&str]) -> Vec<(GtsId, JsonDocument)> {
    ids.iter()
        .map(|id| (GtsId::try_new(id).expect("valid id"), json!({"title": id})))
        .collect()
}

/// A publication of `ids` by `my-gear` 1.0.0, from a manifest.
fn publication<'a>(registry: &'a MockTypesRegistry, ids: &[&str]) -> Publication<'a> {
    Publication::from_manifest(registry, "my-gear", "1.0.0", &manifest(ids))
        .expect("valid manifest")
}

fn is_cancelled(error: &crate::PublicationError) -> bool {
    matches!(error.canonical_error(), CanonicalError::Cancelled { .. }) && !error.is_transient()
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

    publication(&registry, &[A])
        .deadline(deadline)
        .publish(&CancellationToken::new())
        .await
        .expect("the caller grants more than the default 120 seconds");

    assert_eq!(registry.content(A), Some(document(A).1));
    assert!(tokio::time::Instant::now() < deadline);
}

#[tokio::test(start_paused = true)]
async fn the_callers_deadline_bounds_all_retries() {
    let registry = MockTypesRegistry::new();
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

    let error = publication(&registry, &[A])
        .deadline(deadline)
        .publish(&CancellationToken::new())
        .await
        .expect_err("the registry never recovers");

    assert_eq!(tokio::time::Instant::now(), deadline);
    assert_eq!(
        error.report().expect("report").stop,
        ReconcileStop::Deadline
    );
    assert!(registry.submissions().is_empty());
}

/// The default budget is measured from `publish`, not from building the publication.
#[tokio::test(start_paused = true)]
async fn the_default_budget_starts_when_publish_runs() {
    let registry = MockTypesRegistry::new();
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    let publication = publication(&registry, &[A]);
    tokio::time::advance(Duration::from_secs(300)).await;
    let started = tokio::time::Instant::now();

    let error = publication
        .publish(&CancellationToken::new())
        .await
        .expect_err("the registry never recovers");

    assert_eq!(started.elapsed(), crate::STARTUP_PUBLICATION_DEADLINE);
    assert_eq!(
        error.report().expect("report").stop,
        ReconcileStop::Deadline
    );
    assert!(registry.calls(Call::BatchGet) > 1, "reads retried");
}

/// An absolute deadline set before a delay stays where the caller put it.
#[tokio::test(start_paused = true)]
async fn an_absolute_deadline_is_not_shifted_by_a_delay_before_publish() {
    let registry = MockTypesRegistry::new();
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let publication = publication(&registry, &[A]).deadline(deadline);
    tokio::time::advance(Duration::from_secs(45)).await;

    let error = publication
        .publish(&CancellationToken::new())
        .await
        .expect_err("the registry never recovers");

    assert_eq!(tokio::time::Instant::now(), deadline);
    assert_eq!(
        error.report().expect("report").stop,
        ReconcileStop::Deadline
    );
}

/// A deadline that already passed ends the publication before any request.
#[tokio::test(start_paused = true)]
async fn an_expired_deadline_makes_no_request() {
    let registry = MockTypesRegistry::new();
    let deadline = tokio::time::Instant::now();
    tokio::time::advance(Duration::from_secs(1)).await;

    let error = publication(&registry, &[A])
        .deadline(deadline)
        .on_pending(|error| panic!("an expired deadline is not retried: {error}"))
        .publish(&CancellationToken::new())
        .await
        .expect_err("the deadline already passed");

    assert_eq!(
        error.report().expect("report").stop,
        ReconcileStop::Deadline
    );
    assert!(error.to_string().contains(A), "{error}");
    assert_eq!(registry.calls(Call::BatchGet), 0);
    assert_eq!(registry.calls(Call::Register), 0);
}

#[tokio::test]
async fn publishes_the_manifest_under_the_given_identity() {
    let registry = MockTypesRegistry::new();
    let documents = [(GtsId::try_new(A).expect("valid id"), json!({"title": "a"}))];

    let result = Publication::from_manifest(&registry, "my-gear", "1.2.3", &documents)
        .expect("valid manifest")
        .publish(&CancellationToken::new())
        .await
        .expect("admitted");

    let Reconciliation::Reconciled(outcomes) = result else {
        panic!("an absent document needs publication");
    };
    assert!(matches!(
        outcomes[&document(A).0],
        ReconcileOutcome::Admitted
    ));
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

    let error = publication(&registry, &[A])
        .publish(&CancellationToken::new())
        .await
        .expect_err("rejected");

    assert!(!error.is_transient(), "{error}");
    assert!(error.to_string().contains(A), "{error}");
}

/// The ids arrive parsed; the publisher version is parsed here.
#[test]
fn invalid_input_is_reported_before_any_request() {
    let registry = MockTypesRegistry::new();
    for version in ["not-semver", "1.0", ""] {
        let Err(error) = Publication::from_manifest(&registry, "my-gear", version, &manifest(&[A]))
        else {
            panic!("{version} is invalid");
        };
        let message = error.to_string();
        assert!(
            message.starts_with(&format!("my-gear: publisher version {version}: ")),
            "{message}"
        );
    }
    assert_eq!(registry.calls(Call::BatchGet), 0);
    assert!(registry.submissions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn publication_retries_share_one_startup_budget() {
    let registry = MockTypesRegistry::new();
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    let started = tokio::time::Instant::now();
    let cancel = CancellationToken::new();

    let result = tokio::time::timeout(
        crate::STARTUP_PUBLICATION_DEADLINE + Duration::from_secs(1),
        publication(&registry, &[A]).publish(&cancel),
    )
    .await
    .expect("publication must finish within its budget");

    let error = result.expect_err("the registry never recovered");
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

    publication(&registry, &[A])
        .on_pending(|pending| {
            assert!(pending.is_transient(), "{pending}");
            let detail = pending.to_string();
            assert!(detail.contains("pending.dependency"), "{detail}");
            assert!(detail.contains("retrying"), "{detail}");
            retries += 1;
            if retries == 2 {
                registry.seed(B, json!({"title": B}));
            }
        })
        .publish(&CancellationToken::new())
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

    let error = publication(&registry, &[A])
        .on_pending(|pending| {
            let detail = pending.to_string();
            assert!(detail.contains("pending.unavailable"), "{detail}");
            retries += 1;
            if retries == 3 {
                reads_at_cancel = registry.calls(Call::BatchGet);
                cancel.cancel();
            }
        })
        .publish(&cancel)
        .await
        .expect_err("cancelled");

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

    publication(&registry, &[A])
        .on_pending(|_| retries += 1)
        .publish(&CancellationToken::new())
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

    let error = publication(&registry, &[A, B])
        .on_pending(|error| panic!("a rejection is not retried: {error}"))
        .publish(&CancellationToken::new())
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

    let error = publication(&registry, &[A, B])
        .on_pending(|pending| {
            retries += 1;
            let detail = pending.to_string();
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
        })
        .publish(&CancellationToken::new())
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

        let error = publication(&registry, &[A])
            .on_pending(|error| panic!("a refusal is not retried: {error}"))
            .publish(&CancellationToken::new())
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

    let error = publication(&registry, &[A])
        .on_pending(|error| panic!("cancellation is not retried: {error}"))
        .publish(&cancel)
        .await
        .expect_err("cancelled");

    assert!(is_cancelled(&error), "{error}");
    assert_eq!(registry.calls(Call::Register), 0);
}

#[tokio::test(start_paused = true)]
async fn publication_backoff_doubles_with_jitter_up_to_thirty_seconds() {
    let registry = MockTypesRegistry::new().completing_after(0);
    registry.depends_on(A, B);
    let cancel = CancellationToken::new();
    let mut retries = Vec::new();
    let error = publication(&registry, &[A])
        .on_pending(|_| {
            retries.push(tokio::time::Instant::now());
            if retries.len() == 8 {
                cancel.cancel();
            }
        })
        .publish(&cancel)
        .await
        .expect_err("cancelled after observing the pauses");

    assert!(is_cancelled(&error));
    assert_eq!(retries.len(), 8);
    for (pair, seconds) in retries.windows(2).zip([1, 2, 4, 8, 16, 30, 30]) {
        let ceiling = Duration::from_secs(seconds);
        assert!((ceiling / 2..=ceiling).contains(&pair[1].duration_since(pair[0])));
    }
}

// ========== Per-gear policy: timeout, retry backoff, batch size ==========

/// A publication against a registry that never answers its reads.
fn unreachable_registry() -> MockTypesRegistry {
    let registry = MockTypesRegistry::new();
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    registry
}

/// Without a caller's deadline, a timeout replaces the startup default, measured from
/// `publish`, not from building the publication.
#[tokio::test(start_paused = true)]
async fn a_timeout_replaces_the_default_budget_from_the_start_of_publish() {
    let registry = unreachable_registry();
    let publication = publication(&registry, &[A]).timeout(Duration::from_secs(5));
    tokio::time::advance(Duration::from_secs(300)).await;
    let started = tokio::time::Instant::now();

    let error = publication
        .publish(&CancellationToken::new())
        .await
        .expect_err("the registry never recovers");

    assert_eq!(started.elapsed(), Duration::from_secs(5));
    assert_eq!(
        error.report().expect("report").stop,
        ReconcileStop::Deadline
    );
}

#[tokio::test(start_paused = true)]
async fn a_timeout_caps_the_callers_deadline() {
    let registry = unreachable_registry();
    let started = tokio::time::Instant::now();

    let error = publication(&registry, &[A])
        .deadline(started + Duration::from_secs(180))
        .timeout(Duration::from_secs(5))
        .publish(&CancellationToken::new())
        .await
        .expect_err("the registry never recovers");

    assert_eq!(started.elapsed(), Duration::from_secs(5));
    assert_eq!(
        error.report().expect("report").stop,
        ReconcileStop::Deadline
    );
}

#[tokio::test(start_paused = true)]
async fn a_timeout_never_extends_the_callers_deadline() {
    for timeout in [Duration::from_secs(60), Duration::MAX] {
        let registry = unreachable_registry();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

        let error = publication(&registry, &[A])
            .deadline(deadline)
            .timeout(timeout)
            .publish(&CancellationToken::new())
            .await
            .expect_err("the registry never recovers");

        assert_eq!(tokio::time::Instant::now(), deadline, "{timeout:?}");
        assert_eq!(
            error.report().expect("report").stop,
            ReconcileStop::Deadline
        );
    }
}

/// No silent fallback to the startup default.
#[tokio::test(start_paused = true)]
async fn an_unrepresentable_timeout_fails_before_any_request() {
    let registry = MockTypesRegistry::new();
    let started = tokio::time::Instant::now();

    let error = publication(&registry, &[A])
        .timeout(Duration::MAX)
        .on_pending(|error| panic!("an invalid budget is not retried: {error}"))
        .publish(&CancellationToken::new())
        .await
        .expect_err("no clock represents the timeout");

    assert!(
        matches!(
            error.canonical_error(),
            CanonicalError::InvalidArgument { .. }
        ),
        "{error}"
    );
    assert!(!error.is_transient(), "{error}");
    assert_eq!(started.elapsed(), Duration::ZERO);
    assert_eq!(registry.calls(Call::BatchGet), 0);
    assert_eq!(registry.calls(Call::Register), 0);
}

#[tokio::test]
async fn the_batch_size_bounds_every_submission() {
    let registry = MockTypesRegistry::new();

    publication(&registry, &[A, B, C])
        .batch_size(std::num::NonZeroUsize::new(2).expect("non-zero"))
        .publish(&CancellationToken::new())
        .await
        .expect("admitted");

    let mut sizes: Vec<_> = registry
        .submissions()
        .iter()
        .map(|(_, request)| request.items.len())
        .collect();
    sizes.sort_unstable();
    assert_eq!(sizes, [1, 2]);
}

#[tokio::test(start_paused = true)]
async fn the_retry_backoff_sets_the_pause_between_passes() {
    let registry = MockTypesRegistry::new().completing_after(0);
    registry.depends_on(A, B);
    let cancel = CancellationToken::new();
    let mut retries = Vec::new();
    let error = publication(&registry, &[A])
        .retry_backoff(Duration::from_millis(100), Duration::from_millis(300))
        .on_pending(|_| {
            retries.push(tokio::time::Instant::now());
            if retries.len() == 5 {
                cancel.cancel();
            }
        })
        .publish(&cancel)
        .await
        .expect_err("cancelled after observing the pauses");

    assert!(is_cancelled(&error));
    for (pair, millis) in retries.windows(2).zip([100, 200, 300, 300]) {
        let ceiling = Duration::from_millis(millis);
        let pause = pair[1].duration_since(pair[0]);
        assert!(
            (ceiling / 2..=ceiling).contains(&pause),
            "{pause:?} for {ceiling:?}"
        );
    }
}

/// `on_pending` changes the builder's type; the policy set before or after it stays.
#[tokio::test(start_paused = true)]
async fn the_policy_survives_setting_on_pending_before_or_after_it() {
    let two = std::num::NonZeroUsize::new(2).expect("non-zero");
    for policy_first in [true, false] {
        let registry = MockTypesRegistry::new().completing_after(0);
        registry.depends_on(A, "gts.cf.test.publication.absent.v1~");
        let started = tokio::time::Instant::now();
        let mut retries = Vec::new();
        let on_pending = |_: &crate::PublicationError| retries.push(tokio::time::Instant::now());
        let base = publication(&registry, &[A, B, C]);
        let error = if policy_first {
            base.timeout(Duration::from_secs(3))
                .retry_backoff(Duration::from_millis(400), Duration::from_millis(400))
                .batch_size(two)
                .on_pending(on_pending)
                .publish(&CancellationToken::new())
                .await
        } else {
            base.on_pending(on_pending)
                .timeout(Duration::from_secs(3))
                .retry_backoff(Duration::from_millis(400), Duration::from_millis(400))
                .batch_size(two)
                .publish(&CancellationToken::new())
                .await
        }
        .expect_err("A waits on a dependency nobody publishes");

        assert_eq!(started.elapsed(), Duration::from_secs(3), "{policy_first}");
        assert_eq!(
            error.report().expect("report").stop,
            ReconcileStop::Deadline
        );
        for pair in retries.windows(2) {
            let pause = pair[1].duration_since(pair[0]);
            assert!(
                (Duration::from_millis(200)..=Duration::from_millis(400)).contains(&pause),
                "{policy_first}: {pause:?}"
            );
        }
        assert!(retries.len() > 3, "{policy_first}: {}", retries.len());
        assert_eq!(
            registry.submissions()[0].1.items.len(),
            2,
            "{policy_first}: the first batch"
        );
    }
}
