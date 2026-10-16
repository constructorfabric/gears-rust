use std::collections::BTreeMap;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use gts::GtsId;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};
use toolkit_security::PlatformSecurityContext;

use super::{
    ReconcileOptions, ReconcileOutcome, ReconcilePendingCause, ReconcileReport, ReconcileStop,
    Reconciliation, reconcile,
};
use crate::item_failure::{AdmissionFailure, AdmissionFailureReason};
use crate::models::{IdempotencyKey, PublisherContext, RegisterEntitiesRequest};
use crate::testing_platform::{Call, Fault, MockTypesRegistry, ProtocolFault};

const A: &str = "gts.cf.test.pkg.a.v1~";
const B: &str = "gts.cf.test.pkg.b.v1~";
const C: &str = "gts.cf.test.pkg.c.v1~";
const BASE: &str = "gts.cf.test.pkg.base.v1~";

fn ctx() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

fn publisher() -> PublisherContext {
    PublisherContext {
        name: "reconcile-test".to_owned(),
        version: "2.1.0".parse().expect("version"),
    }
}

fn options() -> ReconcileOptions {
    ReconcileOptions::default()
}

fn gid(id: &str) -> GtsId {
    GtsId::try_new(id).expect("a valid identifier")
}

fn doc(id: &str) -> (GtsId, Value) {
    (gid(id), json!({ "title": id }))
}

type Answer = Result<Reconciliation, CanonicalError>;

#[tokio::test(start_paused = true)]
async fn a_failed_reconciliation_returns_a_structured_canonical_error() {
    let fake = MockTypesRegistry::new();
    fake.reject(A, AdmissionFailureReason::InvalidSchema);
    let result: Result<Reconciliation, CanonicalError> = reconcile(
        &fake,
        &ctx(),
        &publisher(),
        &[doc(A), doc(B)],
        &options(),
        &CancellationToken::new(),
    )
    .await;
    let error = result.expect_err("one entity was rejected");
    let CanonicalError::FailedPrecondition { ctx, .. } = &error else {
        panic!("expected a structured failure: {error:?}");
    };
    assert_eq!(ctx.violations[0].type_, "RECONCILIATION_INCOMPLETE");
    assert_eq!(ctx.violations[0].subject, "completed");
    assert_eq!(ctx.violations.len(), 3, "stop and one outcome per entity");
}

async fn run(fake: &MockTypesRegistry, desired: Vec<(GtsId, Value)>) -> Answer {
    run_with(fake, desired, &options()).await
}

async fn run_with(
    fake: &MockTypesRegistry,
    desired: Vec<(GtsId, Value)>,
    options: &ReconcileOptions,
) -> Answer {
    reconcile(
        fake,
        &ctx(),
        &publisher(),
        &desired,
        options,
        &CancellationToken::new(),
    )
    .await
}

/// The outcomes by identifier text, for indexing by the test constants.
fn by_text(
    outcomes: std::collections::HashMap<GtsId, ReconcileOutcome>,
) -> BTreeMap<String, ReconcileOutcome> {
    outcomes
        .into_iter()
        .map(|(id, outcome)| (id.id().to_owned(), outcome))
        .collect()
}

/// Assert all submitted identifiers were admitted.
fn admitted(result: Answer) -> BTreeMap<String, ReconcileOutcome> {
    let outcomes = match result {
        Ok(Reconciliation::Reconciled(outcomes)) => by_text(outcomes),
        other => panic!("expected every identifier admitted after a submission, got {other:?}"),
    };
    assert!(
        outcomes
            .values()
            .all(|o| matches!(o, ReconcileOutcome::Admitted)),
        "success means every identifier is admitted: {outcomes:?}"
    );
    outcomes
}

/// Extract the failure's stop reason and outcomes.
fn unsettled(result: Answer) -> (ReconcileStop, BTreeMap<String, ReconcileOutcome>) {
    match result {
        Err(error) => {
            let report = ReconcileReport::from_canonical(&error).expect("reconciliation report");
            let outcomes = by_text(report.outcomes);
            assert!(
                outcomes
                    .values()
                    .any(|o| !matches!(o, ReconcileOutcome::Admitted)),
                "an unsettled report names something not admitted: {outcomes:?}"
            );
            (report.stop, outcomes)
        }
        other => panic!("expected an unsettled report, got {other:?}"),
    }
}

/// Assert the failure's stop reason.
fn unsettled_by(result: Answer, stop: ReconcileStop) -> BTreeMap<String, ReconcileOutcome> {
    let (stopped, outcomes) = unsettled(result);
    assert_eq!(stopped, stop, "{outcomes:?}");
    outcomes
}

fn caller_cancelled(result: &Answer) -> bool {
    matches!(result, Err(CanonicalError::Cancelled { .. }))
}

fn reason(outcome: &ReconcileOutcome) -> String {
    let error = match outcome {
        ReconcileOutcome::Rejected(e)
        | ReconcileOutcome::Pending(
            ReconcilePendingCause::Dependency(e)
            | ReconcilePendingCause::Conflict(e)
            | ReconcilePendingCause::Unavailable(e)
            | ReconcilePendingCause::Refused(e),
        ) => e,
        ReconcileOutcome::Admitted => return "admitted".to_owned(),
        ReconcileOutcome::Pending(ReconcilePendingCause::Skipped) => return "skipped".to_owned(),
    };
    AdmissionFailure::from_canonical(error)
        .map_or_else(|| format!("{error:?}"), |f| f.reason.as_wire().to_owned())
}

/// Waits, boundedly, for the spawned reconciliation's first submission; a task that ends
/// before submitting fails the test instead of leaving it polling forever.
async fn first_submission<T>(fake: &MockTypesRegistry, task: &tokio::task::JoinHandle<T>) {
    tokio::time::timeout(Duration::from_secs(60), async {
        while fake.submissions().is_empty() {
            assert!(
                !task.is_finished(),
                "reconciliation ended before its first submission"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("reconciliation submits within the bound");
}

fn keys(submissions: &[(IdempotencyKey, RegisterEntitiesRequest)]) -> Vec<String> {
    submissions
        .iter()
        .map(|(k, _)| k.as_str().to_owned())
        .collect()
}

#[tokio::test]
async fn everything_already_matching_is_up_to_date_without_a_submission() {
    let fake = MockTypesRegistry::new();
    let (id, content) = doc(A);
    fake.seed(id.id(), content.clone());

    let result = run(&fake, vec![(id, content)]).await;

    assert!(matches!(result, Ok(Reconciliation::UpToDate)), "{result:?}");
    assert!(fake.submissions().is_empty());
}

#[tokio::test]
async fn only_supplied_documents_are_reconciled() {
    let fake = MockTypesRegistry::new();
    fake.seed(B, json!({ "unrelated": true }));

    let outcomes = admitted(run(&fake, vec![doc(A)]).await);

    assert_eq!(outcomes.len(), 1);
    let submitted: Vec<String> = fake
        .submissions()
        .iter()
        .flat_map(|(_, r)| r.items.iter().map(|i| i.gts_id.to_string()))
        .collect();
    assert_eq!(submitted, [A]);
    assert_eq!(
        fake.content(B),
        Some(json!({ "unrelated": true })),
        "neither submitted nor deleted"
    );
}

#[tokio::test]
async fn an_update_carries_the_read_version_and_the_callers_publisher() {
    let fake = MockTypesRegistry::new();
    fake.seed(A, json!({ "old": true }));

    admitted(run(&fake, vec![doc(A)]).await);

    let submissions = fake.submissions();
    assert_eq!(submissions[0].1.items[0].expected_resource_version, Some(1));
    assert_eq!(submissions[0].1.publisher, publisher());
}

#[tokio::test(start_paused = true)]
async fn a_dependency_published_later_is_picked_up_on_the_next_pass_under_a_new_key() {
    // Decided at submit, so the base is seeded only after the first outcome.
    let fake = Arc::new(MockTypesRegistry::new().completing_after(0));
    fake.depends_on(A, BASE);
    let desired = vec![(gid(A), json!({}))];
    let task = {
        let fake = Arc::clone(&fake);
        tokio::spawn(async move { run(&fake, desired).await })
    };
    first_submission(&fake, &task).await;

    fake.seed(BASE, json!({}));

    admitted(task.await.expect("joins"));
    let keys = keys(&fake.submissions());
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1], "a new pass takes a new key");
}

#[tokio::test(start_paused = true)]
async fn a_concurrent_publisher_of_identical_content_ends_admitted_through_a_re_read() {
    let fake = MockTypesRegistry::new();
    let (id, content) = doc(A);
    fake.race_next_submit(id.id(), content.clone());

    admitted(run(&fake, vec![(id, content)]).await);

    assert_eq!(fake.submissions().len(), 1, "the re-read found it equal");
}

#[tokio::test(start_paused = true)]
async fn two_publishers_of_identical_content_both_end_admitted() {
    let fake = Arc::new(MockTypesRegistry::new());

    let (first, second) = tokio::join!(run(&fake, vec![doc(A)]), run(&fake, vec![doc(A)]));

    for result in [first, second] {
        if !matches!(result, Ok(Reconciliation::UpToDate)) {
            admitted(result);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_conflict_re_reads_and_retries_with_the_new_precondition_and_a_new_key() {
    let fake = MockTypesRegistry::new();
    fake.seed(A, json!({ "v": 1 }));
    fake.race_next_submit(A, json!({ "v": "someone else" }));

    admitted(run(&fake, vec![doc(A)]).await);

    let submissions = fake.submissions();
    assert_eq!(submissions.len(), 2);
    assert_eq!(submissions[0].1.items[0].expected_resource_version, Some(1));
    assert_eq!(submissions[1].1.items[0].expected_resource_version, Some(2));
    assert_ne!(submissions[0].0, submissions[1].0);
}

#[tokio::test(start_paused = true)]
async fn a_lost_receipt_is_recovered_under_the_same_key_and_request() {
    let fake = MockTypesRegistry::new();
    fake.protocol_fault(ProtocolFault::LoseReadBacks(1));

    admitted(run(&fake, vec![doc(A)]).await);

    let submissions = fake.submissions();
    assert_eq!(submissions.len(), 2);
    assert_eq!(
        submissions[0].0, submissions[1].0,
        "no new key for a lost response"
    );
    assert_eq!(
        submissions[0].1, submissions[1].1,
        "and the identical request"
    );
}

#[tokio::test(start_paused = true)]
async fn a_permanently_invalid_document_is_rejected_and_not_retried() {
    let fake = MockTypesRegistry::new();
    fake.reject(A, AdmissionFailureReason::InvalidSchema);

    let outcomes = unsettled_by(
        run(&fake, vec![(gid(A), json!({}))]).await,
        ReconcileStop::Completed,
    );

    assert!(matches!(outcomes[A], ReconcileOutcome::Rejected(_)));
    assert_eq!(reason(&outcomes[A]), "invalid_schema");
    assert_eq!(fake.submissions().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn an_oversized_submission_stops_after_one_request() {
    let fake = MockTypesRegistry::new().with_max_batch(2);
    let desired: Vec<_> = (0..5)
        .map(|n| doc(&format!("gts.cf.test.pkg.n{n}.v1~")))
        .collect();

    let outcomes = unsettled_by(run(&fake, desired).await, ReconcileStop::InvalidInput);

    assert_eq!(
        fake.calls(Call::Register),
        1,
        "the validation refusal ends the call"
    );
    assert_eq!(submitted_ids(&fake)[0].len(), 5);
    assert_eq!(outcomes.len(), 5);
    for (id, outcome) in &outcomes {
        let ReconcileOutcome::Pending(ReconcilePendingCause::Refused(
            CanonicalError::InvalidArgument {
                ctx: InvalidArgument::FieldViolations { field_violations },
                resource_name: None,
                ..
            },
        )) = outcome
        else {
            panic!("{id}: the registry's own size refusal: {outcome:?}");
        };
        assert_eq!(field_violations[0].field, crate::field::ITEMS_FIELD);
        assert!(
            field_violations[0].description.contains("limit of 2"),
            "{field_violations:?}"
        );
    }
}

#[tokio::test]
async fn candidates_go_out_in_batches_of_the_configured_size() {
    let fake = MockTypesRegistry::new();
    let desired: Vec<_> = (0..5)
        .map(|n| doc(&format!("gts.cf.test.pkg.n{n}.v1~")))
        .collect();
    let options = ReconcileOptions {
        batch_size: NonZeroUsize::new(2).expect("non-zero"),
        ..options()
    };

    admitted(run_with(&fake, desired, &options).await);

    let submissions = fake.submissions();
    let sizes: Vec<usize> = submissions.iter().map(|(_, r)| r.items.len()).collect();
    assert_eq!(sizes, [2, 2, 1]);
    let mut keys = keys(&submissions);
    keys.dedup();
    assert_eq!(keys.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn later_batches_proceed_when_an_earlier_one_waits_on_a_dependency() {
    let fake = MockTypesRegistry::new();
    fake.depends_on(A, BASE);
    let options = ReconcileOptions {
        batch_size: NonZeroUsize::new(1).expect("non-zero"),
        passes: NonZeroU32::new(2).expect("non-zero"),
        ..options()
    };

    let outcomes = unsettled_by(
        run_with(&fake, vec![(gid(A), json!({})), doc(B)], &options).await,
        ReconcileStop::PassesExhausted,
    );

    assert!(matches!(outcomes[B], ReconcileOutcome::Admitted));
    assert!(matches!(
        outcomes[A],
        ReconcileOutcome::Pending(ReconcilePendingCause::Dependency(_))
    ));
    assert_eq!(reason(&outcomes[A]), "dependency_not_found");
}

#[tokio::test]
async fn a_conflicting_duplicate_alone_is_reported_not_up_to_date() {
    let fake = MockTypesRegistry::new();

    let outcomes = unsettled_by(
        run(
            &fake,
            vec![(gid(C), json!({ "one": 1 })), (gid(C), json!({ "two": 2 }))],
        )
        .await,
        ReconcileStop::Completed,
    );

    assert!(matches!(outcomes[C], ReconcileOutcome::Rejected(_)));
    assert!(fake.submissions().is_empty());
}

#[tokio::test]
async fn an_equal_document_beside_a_rejected_one_is_reported_with_it() {
    let fake = MockTypesRegistry::new();
    let (id, content) = doc(A);
    fake.seed(id.id(), content.clone());

    let outcomes = unsettled_by(
        run(
            &fake,
            vec![
                (id, content),
                (gid(C), json!({ "one": 1 })),
                (gid(C), json!({ "two": 2 })),
            ],
        )
        .await,
        ReconcileStop::Completed,
    );

    assert!(matches!(outcomes[A], ReconcileOutcome::Admitted));
    assert!(matches!(outcomes[C], ReconcileOutcome::Rejected(_)));
    assert!(fake.submissions().is_empty());
}

#[tokio::test]
async fn an_identifier_declared_twice_differently_is_rejected_and_identical_twins_collapse() {
    let fake = MockTypesRegistry::new();

    let outcomes = unsettled_by(
        run(
            &fake,
            vec![
                (gid(A), json!({ "one": 1 })),
                (gid(A), json!({ "two": 2 })),
                doc(B),
                doc(B),
            ],
        )
        .await,
        ReconcileStop::Completed,
    );

    assert!(matches!(outcomes[A], ReconcileOutcome::Rejected(_)));
    assert!(matches!(outcomes[B], ReconcileOutcome::Admitted));
    let submitted: usize = fake.submissions().iter().map(|(_, r)| r.items.len()).sum();
    assert_eq!(submitted, 1);
}

#[tokio::test]
async fn nothing_desired_is_up_to_date() {
    let fake = MockTypesRegistry::new();
    assert!(matches!(
        run(&fake, Vec::new()).await,
        Ok(Reconciliation::UpToDate)
    ));
}

#[tokio::test]
async fn an_unrepresentable_deadline_is_refused() {
    let fake = MockTypesRegistry::new();
    let options = ReconcileOptions {
        deadline: Duration::MAX,
        ..options()
    };

    let error = reconcile(
        &fake,
        &ctx(),
        &publisher(),
        &[doc(C)],
        &options,
        &CancellationToken::new(),
    )
    .await
    .expect_err("refused");

    assert!(
        matches!(error, CanonicalError::InvalidArgument { .. }),
        "{error:?}"
    );
    assert!(fake.submissions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_unreachable_registry_leaves_identifiers_pending_unavailable() {
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::on(Call::Register).delay(Duration::from_secs(3600)));
    let options = ReconcileOptions {
        deadline: Duration::from_secs(5),
        ..options()
    };

    let outcomes = unsettled_by(
        run_with(&fake, vec![doc(A)], &options).await,
        ReconcileStop::Deadline,
    );

    assert!(matches!(
        outcomes[A],
        ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn an_equal_identifier_settles_at_once_and_a_later_read_failure_cannot_downgrade_it() {
    let fake = MockTypesRegistry::new();
    fake.depends_on(B, BASE);
    let (a, content) = doc(A);
    fake.seed(a.id(), content.clone());
    fake.inject(
        Fault::on(Call::BatchGet)
            .from(2)
            .fail(CanonicalError::service_unavailable().create()),
    );

    let outcomes = unsettled_by(
        run(&fake, vec![(a, content), (gid(B), json!({}))]).await,
        ReconcileStop::ReadFailed,
    );

    assert!(
        matches!(outcomes[A], ReconcileOutcome::Admitted),
        "{:?}",
        outcomes[A]
    );
    assert!(matches!(
        outcomes[B],
        ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(
            CanonicalError::ServiceUnavailable { .. }
        ))
    ));
    let submitted: Vec<String> = fake
        .submissions()
        .iter()
        .flat_map(|(_, r)| r.items.iter().map(|i| i.gts_id.to_string()))
        .collect();
    assert_eq!(submitted, [B], "the settled identifier is never submitted");
}

#[tokio::test(start_paused = true)]
async fn an_equal_identifier_changed_after_settling_is_not_reread_or_resubmitted() {
    let fake = Arc::new(MockTypesRegistry::new().completing_after(0));
    fake.depends_on(B, BASE);
    let (a, content) = doc(A);
    fake.seed(a.id(), content.clone());
    let desired = vec![(a, content), (gid(B), json!({}))];
    let task = {
        let fake = Arc::clone(&fake);
        tokio::spawn(async move { run(&fake, desired).await })
    };
    first_submission(&fake, &task).await;

    fake.seed(A, json!({ "changed": "by someone else" }));
    fake.seed(BASE, json!({}));

    let outcomes = admitted(task.await.expect("joins"));
    assert_eq!(outcomes.len(), 2);
    assert!(
        fake.submissions()
            .iter()
            .all(|(_, r)| r.items.iter().all(|i| i.gts_id.to_string() == B))
    );
}

#[tokio::test]
async fn an_incomplete_read_submits_nothing() {
    for fault in [ProtocolFault::DropAnswers, ProtocolFault::StripOrigin] {
        let fake = MockTypesRegistry::new();
        fake.seed(A, json!({ "old": true }));
        fake.protocol_fault(fault);
        let options = ReconcileOptions {
            passes: NonZeroU32::new(1).expect("non-zero"),
            ..options()
        };

        let outcomes = unsettled_by(
            run_with(&fake, vec![doc(A)], &options).await,
            ReconcileStop::ReadFailed,
        );

        assert!(
            matches!(
                outcomes[A],
                ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(_))
            ),
            "{fault:?}: {:?}",
            outcomes[A]
        );
        assert!(fake.submissions().is_empty(), "{fault:?}: no POST");
    }
}

#[tokio::test(start_paused = true)]
async fn another_publishers_entity_is_rejected() {
    let fake = MockTypesRegistry::new();
    fake.reject(A, AdmissionFailureReason::PublisherMismatch);
    fake.seed(A, json!({ "theirs": true }));

    let outcomes = unsettled_by(
        run(&fake, vec![(gid(A), json!({}))]).await,
        ReconcileStop::Completed,
    );

    assert!(
        matches!(outcomes[A], ReconcileOutcome::Rejected(_)),
        "{:?}",
        outcomes[A]
    );
    assert_eq!(reason(&outcomes[A]), "publisher_mismatch");
}

#[tokio::test(start_paused = true)]
async fn an_oversized_backoff_is_capped_by_the_deadline() {
    let fake = MockTypesRegistry::new();
    fake.depends_on(A, BASE);
    let options = ReconcileOptions {
        retry_backoff: Duration::MAX,
        retry_backoff_max: Duration::MAX,
        deadline: Duration::from_secs(5),
        ..options()
    };
    let started = tokio::time::Instant::now();

    let outcomes = unsettled_by(
        run_with(&fake, vec![(gid(A), json!({}))], &options).await,
        ReconcileStop::Deadline,
    );

    assert!(started.elapsed() <= Duration::from_secs(5));
    assert!(
        matches!(outcomes[A], ReconcileOutcome::Pending(_)),
        "{:?}",
        outcomes[A]
    );
}

#[tokio::test(start_paused = true)]
async fn a_completed_operation_reporting_no_items_keeps_every_identifier_pending() {
    let fake = MockTypesRegistry::new();
    fake.protocol_fault(ProtocolFault::DropOperationItems);
    // One pass: a second would re-read and legitimately find the writes applied.
    let options = ReconcileOptions {
        passes: NonZeroU32::new(1).expect("non-zero"),
        ..options()
    };

    let outcomes = unsettled_by(
        run_with(&fake, vec![doc(A), doc(B)], &options).await,
        ReconcileStop::PassesExhausted,
    );

    assert_eq!(outcomes.len(), 2, "every desired identifier is retained");
    assert!(
        outcomes.values().all(|o| matches!(
            o,
            ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(_))
        )),
        "{outcomes:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn no_batch_is_posted_once_the_budget_is_spent() {
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::on(Call::Register).delay(Duration::from_secs(3)));
    let options = ReconcileOptions {
        batch_size: NonZeroUsize::new(1).expect("non-zero"),
        deadline: Duration::from_secs(5),
        ..options()
    };

    let outcomes = unsettled_by(
        run_with(&fake, vec![doc(A), doc(B), doc(C)], &options).await,
        ReconcileStop::Deadline,
    );

    assert_eq!(
        fake.submissions().len(),
        1,
        "the second outlives the budget, the third never starts"
    );
    assert!(matches!(outcomes[A], ReconcileOutcome::Admitted));
    for id in [B, C] {
        assert!(matches!(
            outcomes[id],
            ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(_))
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn a_zero_budget_reconciliation_submits_nothing() {
    let fake = MockTypesRegistry::new();
    let options = ReconcileOptions {
        deadline: Duration::ZERO,
        ..options()
    };

    let outcomes = unsettled_by(
        run_with(&fake, vec![doc(A)], &options).await,
        ReconcileStop::Deadline,
    );

    assert!(matches!(
        outcomes[A],
        ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(_))
    ));
    assert!(fake.submissions().is_empty());
    assert_eq!(fake.calls(Call::BatchGet), 0);
}

fn one_pass() -> ReconcileOptions {
    ReconcileOptions {
        passes: NonZeroU32::MIN,
        ..options()
    }
}

#[tokio::test(start_paused = true)]
async fn permanent_submission_refusals_stop_before_the_next_batch_or_pass() {
    use crate::gts::TypeResource;
    use toolkit_canonical_errors::{Http, Problem};

    for error in [
        TypeResource::invalid_argument()
            .with_constraint("bad request")
            .create(),
        CanonicalError::unauthenticated()
            .with_reason("TOKEN_INVALID")
            .create(),
        TypeResource::permission_denied()
            .with_reason("DENIED")
            .create(),
        TypeResource::not_found("unknown resource")
            .with_resource(A)
            .create(),
        TypeResource::already_exists("key conflict")
            .with_resource(A)
            .create(),
        TypeResource::resource_exhausted("quota exceeded")
            .with_quota_violation("batch", "quota")
            .create(),
        TypeResource::failed_precondition()
            .with_precondition_violation("request", "refused", "REQUEST_REFUSED")
            .create(),
        TypeResource::out_of_range("bad range")
            .with_field_violation("items", "range", "OUT_OF_RANGE")
            .create(),
        TypeResource::cancelled().create(),
        TypeResource::invalid_argument()
            .with_constraint("unprocessable")
            .with_override(Http::status_code(422))
            .create(),
    ] {
        let expected = serde_json::to_value(Problem::from_error(&error).unwrap()).unwrap();
        let fake = MockTypesRegistry::new();
        fake.inject(Fault::on(Call::Register).first(1).fail(error));
        let options = ReconcileOptions {
            batch_size: NonZeroUsize::new(2).unwrap(),
            ..options()
        };
        let outcomes = unsettled_by(
            run_with(&fake, vec![doc(A), doc(B), doc(C)], &options).await,
            ReconcileStop::InvalidInput,
        );
        for id in [A, B] {
            let ReconcileOutcome::Pending(ReconcilePendingCause::Refused(error)) = &outcomes[id]
            else {
                panic!("request refusal: {:?}", outcomes[id]);
            };
            assert_eq!(
                serde_json::to_value(Problem::from_error(error).unwrap()).unwrap(),
                expected
            );
        }
        assert!(skipped(&outcomes[C]));
        assert_eq!(fake.calls(Call::Register), 1);
        assert_eq!(fake.calls(Call::BatchGet), 1);
        assert_eq!(fake.calls(Call::GetOperation), 0);
    }
}

fn throttled(seconds: u64) -> CanonicalError {
    crate::gts::TypeResource::resource_exhausted("rate limit")
        .with_quota_violation("zone", "rate limit")
        .with_quota_violation_retry_after_seconds(seconds)
        .create()
}

#[tokio::test(start_paused = true)]
async fn throttling_honors_the_hint_and_reuses_the_identical_submission() {
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::on(Call::Register).first(4).fail(throttled(2)));
    let started = tokio::time::Instant::now();

    admitted(run_with(&fake, vec![doc(A)], &one_pass()).await);

    assert!(started.elapsed() >= Duration::from_secs(8));
    let submissions = fake.submissions();
    assert_eq!(submissions.len(), 5);
    assert!(
        submissions
            .iter()
            .all(|submission| *submission == submissions[0])
    );
    assert_eq!(fake.calls(Call::BatchGet), 1);
}

#[tokio::test(start_paused = true)]
async fn throttling_wait_is_bounded_by_the_whole_call_deadline() {
    // A later pass reports the expired budget; the last pass keeps the registry's answer.
    for (what, passes, budget_reported) in [
        ("default passes", options(), true),
        ("one pass", one_pass(), false),
    ] {
        let fake = MockTypesRegistry::new();
        fake.inject(Fault::on(Call::Register).fail(throttled(30)));
        let options = ReconcileOptions {
            deadline: Duration::from_secs(5),
            ..passes
        };
        let started = tokio::time::Instant::now();

        let outcomes = unsettled_by(
            run_with(&fake, vec![doc(A)], &options).await,
            ReconcileStop::Deadline,
        );

        assert_eq!(started.elapsed(), Duration::from_secs(5), "{what}");
        let ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(error)) = &outcomes[A]
        else {
            panic!("{what}: temporary throttling: {:?}", outcomes[A]);
        };
        assert!(
            if budget_reported {
                matches!(error, CanonicalError::DeadlineExceeded { .. })
            } else {
                matches!(error, CanonicalError::ResourceExhausted { .. })
            },
            "{what}: {error:?}"
        );
        assert_eq!(fake.calls(Call::Register), 1, "{what}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_zero_throttling_hint_is_retried_with_backoff_not_refused() {
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::on(Call::Register).first(3).fail(throttled(0)));
    let started = tokio::time::Instant::now();

    admitted(run_with(&fake, vec![doc(A)], &one_pass()).await);

    // Jittered pauses of at least 50, 100 and 200 ms: no busy loop.
    assert!(started.elapsed() >= Duration::from_millis(350));
    let submissions = fake.submissions();
    assert_eq!(submissions.len(), 4);
    assert!(
        submissions
            .iter()
            .all(|submission| *submission == submissions[0])
    );
}

#[tokio::test(start_paused = true)]
async fn throttling_wait_is_cancelled_by_the_callers_token() {
    let fake = Arc::new(MockTypesRegistry::new());
    fake.inject(Fault::on(Call::Register).fail(throttled(30)));
    let cancel = CancellationToken::new();
    let task = {
        let fake = Arc::clone(&fake);
        let cancel = cancel.clone();
        tokio::spawn(async move {
            reconcile(&*fake, &ctx(), &publisher(), &[doc(A)], &options(), &cancel).await
        })
    };
    first_submission(&fake, &task).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    cancel.cancel();
    assert!(caller_cancelled(&task.await.expect("joins")));
    assert_eq!(fake.calls(Call::Register), 1);
}

#[tokio::test(start_paused = true)]
async fn throttling_uses_the_longest_quota_hint() {
    let error = crate::gts::TypeResource::resource_exhausted("rate limits")
        .with_quota_violation("one", "rate limit")
        .with_quota_violation_retry_after_seconds(2)
        .with_quota_violation("two", "rate limit")
        .with_quota_violation_retry_after_seconds(5)
        .create();
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::on(Call::Register).first(1).fail(error));
    let started = tokio::time::Instant::now();
    admitted(run_with(&fake, vec![doc(A)], &one_pass()).await);
    assert!(started.elapsed() >= Duration::from_secs(5));
}

#[tokio::test(start_paused = true)]
async fn retries_follow_the_canonical_category_not_its_http_status() {
    use toolkit_canonical_errors::Http;

    for (what, error, wait) in [
        (
            "aborted",
            crate::gts::TypeResource::aborted("request aborted")
                .with_reason("REQUEST_ABORTED")
                .create(),
            Duration::ZERO,
        ),
        (
            "hinted quota answered as 400",
            crate::gts::TypeResource::resource_exhausted("rate limit")
                .with_quota_violation("zone", "rate limit")
                .with_quota_violation_retry_after_seconds(7)
                .with_override(Http::status_code(400))
                .create(),
            Duration::from_secs(7),
        ),
    ] {
        let fake = MockTypesRegistry::new();
        fake.inject(Fault::on(Call::Register).first(1).fail(error));
        let started = tokio::time::Instant::now();

        admitted(run_with(&fake, vec![doc(A)], &one_pass()).await);

        assert!(started.elapsed() >= wait, "{what}");
        let submissions = fake.submissions();
        assert_eq!(submissions.len(), 2, "{what}");
        assert_eq!(
            submissions[0], submissions[1],
            "{what}: the same key and request"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn throttling_does_not_consume_the_transport_failure_budget() {
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::on(Call::Register).first(2).fail(throttled(1)));
    for attempt in [3, 4] {
        fake.inject(
            Fault::on(Call::Register)
                .nth(attempt)
                .fail(CanonicalError::internal("temporary").create()),
        );
    }
    admitted(run_with(&fake, vec![doc(A)], &one_pass()).await);
    let submissions = fake.submissions();
    assert_eq!(submissions.len(), 5);
    assert!(
        submissions
            .iter()
            .all(|submission| *submission == submissions[0])
    );
}

#[tokio::test(start_paused = true)]
async fn a_policy_refusal_rejects_its_candidate_and_stops_with_unjudged_batch_mates() {
    let error = crate::gts::TypeResource::failed_precondition()
        .with_resource(B)
        .with_precondition_violation(
            "<default>",
            "vendor denied",
            crate::precondition::REGISTRATION_POLICY_ALLOWED_VENDORS,
        )
        .create();
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::on(Call::Register).first(1).fail(error.clone()));
    let outcomes = unsettled_by(
        run(&fake, vec![doc(A), doc(B), doc(C)]).await,
        ReconcileStop::InvalidInput,
    );
    let ReconcileOutcome::Rejected(actual) = &outcomes[B] else {
        panic!("named policy refusal: {:?}", outcomes[B]);
    };
    assert_eq!(format!("{actual:?}"), format!("{error:?}"));
    assert!(skipped(&outcomes[A]));
    assert!(skipped(&outcomes[C]));
    assert_eq!(fake.calls(Call::Register), 1);
    assert_eq!(fake.calls(Call::BatchGet), 1);
}

#[tokio::test(start_paused = true)]
async fn transport_retries_stop_at_their_bound_under_one_key() {
    let fake = MockTypesRegistry::new();
    fake.protocol_fault(ProtocolFault::LoseReadBacks(5));

    let outcomes = unsettled_by(
        run_with(&fake, vec![doc(A)], &one_pass()).await,
        ReconcileStop::PassesExhausted,
    );

    assert!(
        matches!(
            outcomes[A],
            ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(_))
        ),
        "{:?}",
        outcomes[A]
    );
    let submissions = fake.submissions();
    assert_eq!(
        submissions.len(),
        3,
        "exactly TRANSPORT_ATTEMPTS submissions"
    );
    assert!(
        submissions.iter().all(|(k, _)| *k == submissions[0].0),
        "every attempt reuses the batch's key"
    );
}

#[tokio::test(start_paused = true)]
async fn an_internal_failure_is_retried_under_the_same_key() {
    let fake = MockTypesRegistry::new();
    fake.inject(
        Fault::on(Call::Register)
            .first(2)
            .fail(CanonicalError::internal("transient").create()),
    );

    admitted(run_with(&fake, vec![doc(A)], &one_pass()).await);

    let submissions = fake.submissions();
    assert_eq!(submissions.len(), 3);
    assert!(submissions.iter().all(|(k, _)| *k == submissions[0].0));
}

#[tokio::test(start_paused = true)]
async fn a_refusal_of_the_call_is_submitted_once_and_stays_pending_as_refused() {
    let fake = MockTypesRegistry::new();
    fake.inject(
        Fault::on(Call::Register).first(1).fail(
            CanonicalError::unauthenticated()
                .with_reason("TOKEN_INVALID")
                .create(),
        ),
    );

    let outcomes = unsettled_by(run(&fake, vec![doc(A)]).await, ReconcileStop::InvalidInput);

    assert!(
        matches!(
            outcomes[A],
            ReconcileOutcome::Pending(ReconcilePendingCause::Refused(
                CanonicalError::Unauthenticated { .. }
            ))
        ),
        "an authentication failure is not an unreachable registry: {:?}",
        outcomes[A]
    );
    assert_eq!(fake.submissions().len(), 1, "a refusal is not retried");
}

#[test]
fn every_exact_reason_maps_to_its_outcome() {
    use super::classify;
    use crate::item_failure::reason;
    use crate::models::RegistrationOutcome;

    let failed = |r: &str| {
        classify(RegistrationOutcome::Failed {
            error: AdmissionFailure::new(
                crate::item_failure::AdmissionFailureReason::from_wire(r),
                "m",
            )
            .into_canonical(A),
        })
    };
    for r in [
        reason::DEPENDENCY_NOT_FOUND,
        reason::BLOCKED_BY_DEPENDENCY,
        reason::BLOCKED_BY_PREDECESSOR,
        reason::MISSING_PREDECESSOR,
    ] {
        assert!(
            matches!(
                failed(r),
                ReconcileOutcome::Pending(ReconcilePendingCause::Dependency(_))
            ),
            "{r}"
        );
    }
    for r in [reason::ALREADY_EXISTS, reason::PRECONDITION_FAILED] {
        assert!(
            matches!(
                failed(r),
                ReconcileOutcome::Pending(ReconcilePendingCause::Conflict(_))
            ),
            "{r}"
        );
    }
    assert!(matches!(
        failed(reason::SYSTEM_FAILURE),
        ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(_))
    ));
    for r in [
        reason::SUPERSEDED,
        reason::PUBLISHER_MISMATCH,
        "invalid_schema",
        "future_reason",
    ] {
        assert!(matches!(failed(r), ReconcileOutcome::Rejected(_)), "{r}");
    }
}

#[test]
fn undecided_or_previewed_items_stay_pending_and_written_ones_are_admitted() {
    use super::classify;
    use crate::models::RegistrationOutcome;

    // A completed committing operation never leaves these: a protocol fault, not a write.
    for outcome in [
        RegistrationOutcome::Pending,
        RegistrationOutcome::Running,
        RegistrationOutcome::WouldSucceed,
    ] {
        assert!(
            matches!(
                classify(outcome.clone()),
                ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(
                    CanonicalError::Aborted { .. }
                ))
            ),
            "{outcome:?}"
        );
    }
    for outcome in [
        RegistrationOutcome::Succeeded {
            resource_version: 1,
        },
        RegistrationOutcome::Unchanged {
            resource_version: 1,
        },
    ] {
        assert!(
            matches!(classify(outcome.clone()), ReconcileOutcome::Admitted),
            "{outcome:?}"
        );
    }
}

/// Measure virtual pass time for an absent dependency; one read per pass.
async fn time_pending_passes(passes: u32, backoff: Duration, max: Duration) -> (Duration, u32) {
    // Completing on submit: no poll interval adds to the passes' pauses.
    let fake = MockTypesRegistry::new().completing_after(0);
    fake.depends_on(A, BASE);
    let options = ReconcileOptions {
        passes: NonZeroU32::new(passes).expect("non-zero"),
        retry_backoff: backoff,
        retry_backoff_max: max,
        deadline: Duration::from_secs(600),
        ..options()
    };
    let started = tokio::time::Instant::now();
    let outcomes = unsettled_by(
        run_with(&fake, vec![(gid(A), json!({}))], &options).await,
        ReconcileStop::PassesExhausted,
    );
    assert!(matches!(
        outcomes[A],
        ReconcileOutcome::Pending(ReconcilePendingCause::Dependency(_))
    ));
    (started.elapsed(), fake.calls(Call::BatchGet))
}

#[tokio::test(start_paused = true)]
async fn the_pause_keeps_doubling_past_any_integer_factor_up_to_its_maximum() {
    // 1 ns doubling 33 times reaches 8.59 s, then 10 s for the last seven pauses: at least
    // half of it, 43.59 s, under jitter. A factor capped at u32::MAX would stop at 4.29 s.
    let (elapsed, reads) =
        time_pending_passes(42, Duration::from_nanos(1), Duration::from_secs(10)).await;

    assert_eq!(reads, 42);
    assert!(elapsed >= Duration::from_secs(43), "{elapsed:?}");
}

#[tokio::test(start_paused = true)]
async fn the_pause_between_passes_doubles_within_its_jitter_envelope() {
    // 200 + 400 + 800 + 1600 ms, each jittered into [half, full].
    let (elapsed, reads) =
        time_pending_passes(5, Duration::from_millis(200), Duration::from_secs(60)).await;

    assert_eq!(reads, 5);
    assert!(
        (Duration::from_millis(1500)..=Duration::from_millis(3000)).contains(&elapsed),
        "{elapsed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_pause_between_passes_is_capped_and_an_oversized_initial_one_clamped() {
    // Three capped pauses take [1.5, 3] s; uncapped doubling takes at least 3.5 s.
    let (elapsed, _) = time_pending_passes(4, Duration::from_secs(1), Duration::from_secs(1)).await;
    assert!(
        (Duration::from_millis(1500)..=Duration::from_secs(3)).contains(&elapsed),
        "{elapsed:?}"
    );

    // An initial pause above the maximum is clamped to it.
    let (elapsed, _) =
        time_pending_passes(2, Duration::from_secs(10), Duration::from_secs(1)).await;
    assert!(elapsed <= Duration::from_secs(1), "{elapsed:?}");
}

// cancellation.

/// A reconciliation the test can cancel, with a pause between passes long enough to land in.
fn spawn_cancellable(
    fake: &Arc<MockTypesRegistry>,
    desired: Vec<(GtsId, Value)>,
) -> (CancellationToken, tokio::task::JoinHandle<Answer>) {
    let cancel = CancellationToken::new();
    let options = ReconcileOptions {
        retry_backoff: Duration::from_secs(3600),
        retry_backoff_max: Duration::from_secs(3600),
        deadline: Duration::from_secs(36_000),
        ..options()
    };
    let task = {
        let (fake, cancel) = (Arc::clone(fake), cancel.clone());
        tokio::spawn(async move {
            reconcile(&*fake, &ctx(), &publisher(), &desired, &options, &cancel).await
        })
    };
    (cancel, task)
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_blocked_read_ends_reconciliation_as_cancelled_without_submitting() {
    let fake = Arc::new(MockTypesRegistry::new());
    fake.inject(Fault::on(Call::BatchGet).delay(Duration::from_secs(3600)));
    let (cancel, task) = spawn_cancellable(&fake, vec![doc(A)]);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        fake.calls(Call::BatchGet),
        1,
        "the read has started and is blocked"
    );

    cancel.cancel();

    let result = task.await.expect("joins");
    assert!(caller_cancelled(&result), "{result:?}");
    assert!(fake.submissions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn cancelling_the_pause_between_passes_ends_reconciliation_without_another_submission() {
    let fake = Arc::new(MockTypesRegistry::new().completing_after(0));
    fake.depends_on(A, BASE);
    let (cancel, task) = spawn_cancellable(&fake, vec![(gid(A), json!({}))]);
    first_submission(&fake, &task).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let reads = fake.calls(Call::BatchGet);

    cancel.cancel();
    // Would let a second pass through, had the pause not ended on cancellation.
    fake.seed(BASE, json!({}));

    let result = task.await.expect("joins");
    assert!(caller_cancelled(&result), "{result:?}");
    assert_eq!(
        fake.submissions().len(),
        1,
        "no pass after the cancellation"
    );
    assert_eq!(fake.calls(Call::BatchGet), reads, "nor its read");
}

// exactly one outcome per candidate.

mod cover {
    use gts::GtsId;

    use super::*;
    use crate::models::{RegisterItem, RegistrationItemResult, RegistrationOutcome};
    use crate::reconcile::cover;

    fn candidate(id: &str) -> RegisterItem {
        RegisterItem {
            gts_id: GtsId::try_new(id).expect("valid identifier"),
            content: json!({}),
            expected_resource_version: None,
            skip_compatibility_check: false,
        }
    }

    fn succeeded(id: &str) -> RegistrationItemResult {
        RegistrationItemResult {
            gts_id: GtsId::try_new(id).expect("valid identifier"),
            outcome: RegistrationOutcome::Succeeded {
                resource_version: 1,
            },
        }
    }

    fn outcome_of(decided: &[(GtsId, ReconcileOutcome)], id: &str) -> ReconcileOutcome {
        decided
            .iter()
            .find(|(gts_id, _)| gts_id.id() == id)
            .map(|(_, outcome)| outcome.clone())
            .expect("every submitted candidate has an outcome")
    }

    #[test]
    fn a_candidate_reported_twice_stays_pending_and_its_neighbour_settles() {
        let decided = cover(
            &[candidate(A), candidate(B)],
            vec![succeeded(A), succeeded(A), succeeded(B)],
        );

        assert_eq!(decided.len(), 2);
        assert!(
            matches!(
                outcome_of(&decided, A),
                ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(_))
            ),
            "a duplicate report is not an admission"
        );
        assert!(matches!(
            outcome_of(&decided, B),
            ReconcileOutcome::Admitted
        ));
    }

    #[test]
    fn an_unrequested_report_is_ignored_and_settles_nothing() {
        let decided = cover(
            &[candidate(A), candidate(B)],
            vec![succeeded(B), succeeded(C)],
        );

        assert_eq!(
            decided.iter().map(|(id, _)| id.id()).collect::<Vec<_>>(),
            [A, B],
            "only the submitted candidates, each once"
        );
        assert!(matches!(
            outcome_of(&decided, A),
            ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(_))
        ));
        assert!(matches!(
            outcome_of(&decided, B),
            ReconcileOutcome::Admitted
        ));
    }
}

#[test]
fn revalidation_exhaustion_is_a_conflict_the_next_pass_retries() {
    use crate::item_failure::AdmissionFailureReason;
    use crate::models::RegistrationOutcome;

    let error = AdmissionFailure::new(
        AdmissionFailureReason::RevalidationExhausted,
        "concurrent writers kept moving the dependency vector",
    )
    .into_canonical(A);

    let outcome = super::classify(RegistrationOutcome::Failed { error });

    assert!(
        matches!(
            outcome,
            ReconcileOutcome::Pending(ReconcilePendingCause::Conflict(_))
        ),
        "{outcome:?}"
    );
}

/// A `Cancelled` answered by the registry, not raised by the caller's token.
fn remote_cancelled() -> CanonicalError {
    crate::gts::OperationResource::cancelled().create()
}

fn refused_as_cancelled(outcome: &ReconcileOutcome) -> bool {
    matches!(
        outcome,
        ReconcileOutcome::Pending(ReconcilePendingCause::Refused(
            CanonicalError::Cancelled { .. }
        ))
    )
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_read_answer_is_reported_pending_not_as_the_callers_cancellation() {
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::on(Call::BatchGet).fail(remote_cancelled()));

    let outcomes = unsettled_by(run(&fake, vec![doc(A)]).await, ReconcileStop::ReadFailed);

    assert!(refused_as_cancelled(&outcomes[A]), "{:?}", outcomes[A]);
    assert_eq!(
        fake.calls(Call::BatchGet),
        1,
        "the call ends at the refused read"
    );
    assert!(fake.submissions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_submit_answer_is_not_retried_and_keeps_the_other_outcomes() {
    let fake = MockTypesRegistry::new();
    fake.inject(
        Fault::on(Call::Register)
            .key(gts::GtsId::try_new(B).expect("valid"))
            .fail(remote_cancelled()),
    );
    let options = ReconcileOptions {
        batch_size: NonZeroUsize::MIN,
        ..options()
    };

    let outcomes = unsettled_by(
        run_with(&fake, vec![doc(A), doc(B)], &options).await,
        ReconcileStop::InvalidInput,
    );

    assert!(
        matches!(outcomes[A], ReconcileOutcome::Admitted),
        "{:?}",
        outcomes[A]
    );
    assert!(refused_as_cancelled(&outcomes[B]), "{:?}", outcomes[B]);
    let b_submissions = fake
        .submissions()
        .iter()
        .filter(|(_, request)| request.items.iter().any(|i| i.gts_id.id() == B))
        .count();
    assert_eq!(b_submissions, 1, "a registry's cancellation is not retried");
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_poll_answer_is_reported_pending_not_as_the_callers_cancellation() {
    let fake = MockTypesRegistry::new().completing_after(1);
    fake.inject(Fault::on(Call::GetOperation).fail(remote_cancelled()));

    let outcomes = unsettled_by(run(&fake, vec![doc(A)]).await, ReconcileStop::Completed);

    assert!(refused_as_cancelled(&outcomes[A]), "{:?}", outcomes[A]);
    assert_eq!(
        fake.calls(Call::Register),
        1,
        "a registry's cancellation is not retried, by transport or by another pass"
    );
}

#[tokio::test(start_paused = true)]
async fn cancelling_while_polling_ends_reconciliation_as_cancelled() {
    let fake = Arc::new(MockTypesRegistry::new().completing_after(u32::MAX));
    let (cancel, task) = spawn_cancellable(&fake, vec![doc(A)]);
    first_submission(&fake, &task).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        fake.calls(Call::GetOperation) > 0,
        "the operation is being polled"
    );

    cancel.cancel();

    let result = task.await.expect("joins");
    assert!(caller_cancelled(&result), "{result:?}");
    assert_eq!(fake.submissions().len(), 1, "nothing is resubmitted");
}

fn invalid(field: &str, resource: Option<&str>) -> CanonicalError {
    let builder = crate::gts::TypeResource::invalid_argument();
    match resource {
        Some(name) => builder
            .with_resource(name.to_owned())
            .with_field_violation(field, "refused", crate::field::VALIDATION_FAILED)
            .create(),
        None => builder
            .with_field_violation(field, "refused", crate::field::VALIDATION_FAILED)
            .create(),
    }
}

fn refused_pending(outcome: &ReconcileOutcome) -> bool {
    matches!(
        outcome,
        ReconcileOutcome::Pending(ReconcilePendingCause::Refused(
            CanonicalError::InvalidArgument { .. }
        ))
    )
}

#[tokio::test(start_paused = true)]
async fn a_refusal_of_the_whole_request_stops_the_call_and_keeps_its_error_pending() {
    for (what, error) in [
        ("the publisher", invalid("publisher", None)),
        (
            "a foreign resource",
            invalid(crate::field::GTS_ID_FIELD, Some(BASE)),
        ),
        (
            "mixed violations",
            crate::gts::TypeResource::invalid_argument()
                .with_field_violation(crate::field::ITEMS_FIELD, "too many", "VALIDATION_FAILED")
                .with_field_violation("publisher", "missing", "VALIDATION_FAILED")
                .create(),
        ),
        (
            "a named candidate mixed with the publisher",
            crate::gts::TypeResource::invalid_argument()
                .with_resource(B.to_owned())
                .with_field_violation(crate::field::ENTITY_FIELD, "bad", "VALIDATION_FAILED")
                .with_field_violation("publisher", "missing", "VALIDATION_FAILED")
                .create(),
        ),
        (
            "a named candidate under an unknown field",
            invalid("publisher", Some(B)),
        ),
    ] {
        let fake = MockTypesRegistry::new();
        fake.inject(Fault::on(Call::Register).fail(error));

        // Every pass is allowed: a refused request is still submitted only once.
        let outcomes = unsettled_by(
            run(&fake, vec![doc(A), doc(B), doc(C)]).await,
            ReconcileStop::InvalidInput,
        );

        assert!(
            outcomes.values().all(refused_pending),
            "{what}: {outcomes:?}"
        );
        assert_eq!(
            fake.calls(Call::Register),
            1,
            "{what}: another pass does not cure it"
        );
    }
}

const D: &str = "gts.cf.test.pkg.d.v1~";
const E: &str = "gts.cf.test.pkg.e.v1~";

/// Refuse every registration that carries `id`, naming it as a candidate refusal does.
fn refuse_candidate(fake: &MockTypesRegistry, id: &str) {
    refuse_candidate_field(fake, id, crate::field::ENTITY_FIELD);
}

/// Refuse every registration that carries `id` with a violation of its `field`.
fn refuse_candidate_field(fake: &MockTypesRegistry, id: &str, field: &str) {
    fake.inject(
        Fault::on(Call::Register)
            .key(gid(id))
            .fail(invalid(field, Some(id))),
    );
}

/// Every submission's key and candidate identifiers, in order.
fn submitted(fake: &MockTypesRegistry) -> Vec<(String, Vec<String>)> {
    fake.submissions()
        .into_iter()
        .map(|(key, request)| {
            (
                key.as_str().to_owned(),
                request
                    .items
                    .iter()
                    .map(|i| i.gts_id.id().to_owned())
                    .collect(),
            )
        })
        .collect()
}

fn assert_one_outcome_each(outcomes: &BTreeMap<String, ReconcileOutcome>, ids: &[&str]) {
    let mut expected: Vec<&str> = ids.to_vec();
    expected.sort_unstable();
    assert_eq!(
        outcomes.keys().map(String::as_str).collect::<Vec<_>>(),
        expected,
        "every desired identifier has exactly one outcome"
    );
}

fn skipped(outcome: &ReconcileOutcome) -> bool {
    matches!(
        outcome,
        ReconcileOutcome::Pending(ReconcilePendingCause::Skipped)
    )
}

fn submitted_ids(fake: &MockTypesRegistry) -> Vec<Vec<String>> {
    submitted(fake).into_iter().map(|(_, ids)| ids).collect()
}

#[tokio::test(start_paused = true)]
async fn a_refused_candidate_stops_the_call_and_its_batch_mates_are_skipped_unjudged() {
    let fake = MockTypesRegistry::new();
    refuse_candidate(&fake, B);

    // Invalid input must stop before the next pass.
    let outcomes = unsettled_by(
        run(&fake, vec![doc(A), doc(B), doc(C), doc(D)]).await,
        ReconcileStop::InvalidInput,
    );

    assert_one_outcome_each(&outcomes, &[A, B, C, D]);
    let ReconcileOutcome::Rejected(error) = &outcomes[B] else {
        panic!("{:?}", outcomes[B]);
    };
    assert_eq!(
        error.resource_name(),
        Some(B),
        "the registry's error is kept as answered"
    );
    for id in [A, C, D] {
        assert!(skipped(&outcomes[id]), "{id}: {:?}", outcomes[id]);
    }
    assert_eq!(
        submitted_ids(&fake),
        [vec![A, B, C, D]],
        "nothing is resubmitted"
    );
    assert_eq!(fake.calls(Call::BatchGet), 1, "no later pass");
}

/// Every candidate field the registry can name classifies its refusal as that
/// candidate's invalid input.
#[tokio::test(start_paused = true)]
async fn a_refusal_naming_any_candidate_field_rejects_that_candidate_and_stops() {
    for field in [
        crate::field::GTS_ID_FIELD,
        crate::field::ENTITY_FIELD,
        crate::field::EXPECTED_RESOURCE_VERSION_FIELD,
        crate::field::SKIP_COMPATIBILITY_CHECK_FIELD,
    ] {
        let fake = MockTypesRegistry::new();
        refuse_candidate_field(&fake, B, field);

        let outcomes = unsettled_by(
            run(&fake, vec![doc(A), doc(B), doc(C)]).await,
            ReconcileStop::InvalidInput,
        );

        assert!(
            matches!(
                outcomes[B],
                ReconcileOutcome::Rejected(CanonicalError::InvalidArgument { .. })
            ),
            "{field}: {:?}",
            outcomes[B]
        );
        for id in [A, C] {
            assert!(skipped(&outcomes[id]), "{field} {id}: {:?}", outcomes[id]);
        }
        assert_eq!(fake.calls(Call::Register), 1, "{field}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_refused_candidate_stops_later_batches_and_keeps_every_earlier_outcome() {
    let fake = MockTypesRegistry::new();
    fake.reject(B, AdmissionFailureReason::InvalidSchema);
    refuse_candidate(&fake, C);
    let options = ReconcileOptions {
        batch_size: NonZeroUsize::MIN,
        ..options()
    };

    let result = run_with(
        &fake,
        vec![doc(A), (gid(B), json!({})), doc(C), doc(D), doc(E)],
        &options,
    )
    .await;

    assert_eq!(
        ReconcileReport::from_canonical(result.as_ref().expect_err("invalid input fails the call"))
            .expect("report")
            .to_string(),
        "reconciliation stopped (invalid_input) with 2 rejected and 2 pending of 5 identifiers"
    );
    let outcomes = unsettled_by(result, ReconcileStop::InvalidInput);
    assert_one_outcome_each(&outcomes, &[A, B, C, D, E]);
    assert!(
        matches!(outcomes[A], ReconcileOutcome::Admitted),
        "{:?}",
        outcomes[A]
    );
    assert_eq!(
        reason(&outcomes[B]),
        "invalid_schema",
        "an earlier admission refusal is kept"
    );
    assert!(
        matches!(
            outcomes[C],
            ReconcileOutcome::Rejected(CanonicalError::InvalidArgument { .. })
        ),
        "{:?}",
        outcomes[C]
    );
    for id in [D, E] {
        assert!(skipped(&outcomes[id]), "{id}: {:?}", outcomes[id]);
    }
    assert_eq!(
        submitted_ids(&fake),
        [vec![A], vec![B], vec![C]],
        "no batch after the refused one"
    );
    assert_eq!(
        fake.content(A),
        Some(json!({ "title": A })),
        "admitted writes stay"
    );
}

#[tokio::test(start_paused = true)]
async fn a_refused_single_candidate_is_rejected_and_nothing_is_resubmitted() {
    let fake = MockTypesRegistry::new();
    refuse_candidate(&fake, A);

    let outcomes = unsettled_by(run(&fake, vec![doc(A)]).await, ReconcileStop::InvalidInput);

    assert!(
        matches!(outcomes[A], ReconcileOutcome::Rejected(_)),
        "{:?}",
        outcomes[A]
    );
    assert_eq!(fake.calls(Call::Register), 1);
}

#[tokio::test(start_paused = true)]
async fn a_format_or_constraint_refusal_stops_the_call_without_rejecting_on_a_guess() {
    for (what, error) in [
        (
            "format",
            crate::gts::TypeResource::invalid_argument()
                .with_format("malformed body")
                .create(),
        ),
        (
            "constraint",
            crate::gts::TypeResource::invalid_argument()
                .with_constraint("violates a constraint")
                .create(),
        ),
        (
            "format naming a candidate",
            crate::gts::TypeResource::invalid_argument()
                .with_resource(A.to_owned())
                .with_format("malformed body")
                .create(),
        ),
    ] {
        let fake = MockTypesRegistry::new();
        fake.inject(Fault::on(Call::Register).first(1).fail(error));
        let options = ReconcileOptions {
            batch_size: NonZeroUsize::new(2).expect("non-zero"),
            ..options()
        };

        let outcomes = unsettled_by(
            run_with(&fake, vec![doc(A), doc(B), doc(C)], &options).await,
            ReconcileStop::InvalidInput,
        );

        for id in [A, B] {
            assert!(
                matches!(
                    &outcomes[id],
                    ReconcileOutcome::Pending(ReconcilePendingCause::Refused(
                        CanonicalError::InvalidArgument { .. }
                    ))
                ),
                "{what} {id}: the request's error, no rejection: {:?}",
                outcomes[id]
            );
        }
        assert!(skipped(&outcomes[C]), "{what}: {:?}", outcomes[C]);
        assert_eq!(
            submitted_ids(&fake),
            [vec![A, B]],
            "{what}: neither resubmitted nor followed"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_size_refusal_of_a_later_batch_keeps_earlier_outcomes_and_skips_the_rest() {
    let fake = MockTypesRegistry::new();
    fake.inject(
        Fault::on(Call::Register)
            .nth(2)
            .fail(invalid(crate::field::ITEMS_FIELD, None)),
    );
    let options = ReconcileOptions {
        batch_size: NonZeroUsize::new(2).expect("non-zero"),
        ..options()
    };

    let outcomes = unsettled_by(
        run_with(
            &fake,
            vec![doc(A), doc(B), doc(C), doc(D), doc(E)],
            &options,
        )
        .await,
        ReconcileStop::InvalidInput,
    );

    for id in [A, B] {
        assert!(
            matches!(outcomes[id], ReconcileOutcome::Admitted),
            "{id}: {:?}",
            outcomes[id]
        );
    }
    for id in [C, D] {
        assert!(refused_pending(&outcomes[id]), "{id}: {:?}", outcomes[id]);
    }
    assert!(skipped(&outcomes[E]), "{:?}", outcomes[E]);
    assert_eq!(submitted_ids(&fake), [vec![A, B], vec![C, D]]);
}

#[tokio::test(start_paused = true)]
async fn a_dry_run_preview_answering_a_committing_registration_is_not_admitted() {
    let fake = MockTypesRegistry::new();
    fake.protocol_fault(ProtocolFault::PreviewSuccesses);

    let outcomes = unsettled_by(
        run_with(&fake, vec![doc(A)], &one_pass()).await,
        ReconcileStop::PassesExhausted,
    );

    assert!(
        matches!(
            outcomes[A],
            ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(
                CanonicalError::Aborted { .. }
            ))
        ),
        "a preview is not a write: {:?}",
        outcomes[A]
    );
}

// the call succeeds only when every identifier is admitted.

#[tokio::test(start_paused = true)]
async fn a_call_admitting_every_submitted_identifier_succeeds_with_each_outcome() {
    let fake = MockTypesRegistry::new();
    let (seeded, content) = doc(A);
    fake.seed(seeded.id(), content.clone());

    let outcomes = admitted(run(&fake, vec![(seeded, content), doc(B), doc(C)]).await);

    assert_one_outcome_each(&outcomes, &[A, B, C]);
    assert_eq!(
        submitted(&fake).len(),
        1,
        "only the differing pair is submitted"
    );
}

#[tokio::test(start_paused = true)]
async fn admission_rejections_fail_the_call_after_every_remaining_batch_is_submitted() {
    let fake = MockTypesRegistry::new();
    fake.reject(B, AdmissionFailureReason::InvalidSchema);
    fake.reject(D, AdmissionFailureReason::PublisherMismatch);
    let options = ReconcileOptions {
        batch_size: NonZeroUsize::MIN,
        ..options()
    };

    let result = run_with(
        &fake,
        vec![
            doc(A),
            (gid(B), json!({})),
            doc(C),
            (gid(D), json!({})),
            doc(E),
        ],
        &options,
    )
    .await;

    assert_eq!(
        ReconcileReport::from_canonical(result.as_ref().expect_err("rejections fail the call"))
            .expect("report")
            .to_string(),
        "reconciliation stopped (completed) with 2 rejected and 0 pending of 5 identifiers"
    );
    let outcomes = unsettled_by(result, ReconcileStop::Completed);
    assert_one_outcome_each(&outcomes, &[A, B, C, D, E]);
    for id in [A, C, E] {
        assert!(
            matches!(outcomes[id], ReconcileOutcome::Admitted),
            "{id}: {:?}",
            outcomes[id]
        );
    }
    assert_eq!(
        reason(&outcomes[B]),
        "invalid_schema",
        "the registry's error is kept"
    );
    assert_eq!(reason(&outcomes[D]), "publisher_mismatch");
    assert_eq!(
        submitted_ids(&fake),
        [vec![A], vec![B], vec![C], vec![D], vec![E]],
        "every batch after an admission refusal is still submitted, once"
    );
    assert_eq!(
        fake.content(A),
        Some(json!({ "title": A })),
        "admitted writes stay"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unattributed_candidate_field_refusal_stops_the_call_without_a_rejection() {
    // Over-long identifiers are not echoed back.
    let error = invalid(crate::field::GTS_ID_FIELD, None);
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::on(Call::Register).first(1).fail(error.clone()));
    let options = ReconcileOptions {
        batch_size: NonZeroUsize::MIN,
        ..options()
    };

    let outcomes = unsettled_by(
        run_with(&fake, vec![doc(A), doc(B), doc(C)], &options).await,
        ReconcileStop::InvalidInput,
    );

    let ReconcileOutcome::Pending(ReconcilePendingCause::Refused(kept)) = &outcomes[A] else {
        panic!("not rejected on a guess: {:?}", outcomes[A]);
    };
    assert_eq!(
        format!("{kept:?}"),
        format!("{error:?}"),
        "the error is kept as answered"
    );
    for id in [B, C] {
        assert!(skipped(&outcomes[id]), "{id}: {:?}", outcomes[id]);
    }
    assert_eq!(fake.calls(Call::Register), 1);
    assert_eq!(submitted_ids(&fake), [vec![A]], "no later batch is sent");
}

#[tokio::test(start_paused = true)]
async fn a_stop_on_invalid_input_keeps_outcomes_known_from_earlier_passes() {
    // Batch size 1: B is queued behind the refused A. Size 2: B is A's batch-mate.
    for (batch_size, refused_submission) in [(1, 3), (2, 2)] {
        let fake = MockTypesRegistry::new().completing_after(0);
        fake.depends_on(A, BASE);
        fake.depends_on(B, BASE);
        // The first submission of the second pass.
        fake.inject(
            Fault::on(Call::Register)
                .nth(refused_submission)
                .fail(invalid(crate::field::ENTITY_FIELD, Some(A))),
        );
        let options = ReconcileOptions {
            batch_size: NonZeroUsize::new(batch_size).expect("non-zero"),
            ..options()
        };

        let outcomes = unsettled_by(
            run_with(
                &fake,
                vec![(gid(A), json!({})), (gid(B), json!({}))],
                &options,
            )
            .await,
            ReconcileStop::InvalidInput,
        );

        assert!(
            matches!(
                outcomes[A],
                ReconcileOutcome::Rejected(CanonicalError::InvalidArgument { .. })
            ),
            "{batch_size}: {:?}",
            outcomes[A]
        );
        assert!(
            matches!(
                outcomes[B],
                ReconcileOutcome::Pending(ReconcilePendingCause::Dependency(_))
            ),
            "{batch_size}: the first pass's outcome, not skipped: {:?}",
            outcomes[B]
        );
        assert_eq!(reason(&outcomes[B]), "dependency_not_found", "{batch_size}");
        assert_eq!(
            fake.calls(Call::Register),
            refused_submission,
            "{batch_size}"
        );
    }
}

// only the submission's own refusal judges the input.

#[tokio::test(start_paused = true)]
async fn an_invalid_argument_answering_a_poll_is_not_a_refusal_of_the_input() {
    for (what, error) in [
        (
            "naming a candidate's field",
            invalid(crate::field::ENTITY_FIELD, Some(A)),
        ),
        ("naming nothing", invalid(crate::field::ITEMS_FIELD, None)),
    ] {
        // Accepted pending: the first poll answers the error.
        let fake = MockTypesRegistry::new().completing_after(1);
        fake.inject(Fault::on(Call::GetOperation).first(1).fail(error.clone()));
        let options = ReconcileOptions {
            batch_size: NonZeroUsize::MIN,
            ..one_pass()
        };

        let outcomes = unsettled_by(
            run_with(&fake, vec![doc(A), doc(B)], &options).await,
            ReconcileStop::Completed,
        );

        let ReconcileOutcome::Pending(ReconcilePendingCause::Refused(kept)) = &outcomes[A] else {
            panic!("{what}: neither rejected nor skipped: {:?}", outcomes[A]);
        };
        assert_eq!(format!("{kept:?}"), format!("{error:?}"), "{what}");
        assert!(
            matches!(outcomes[B], ReconcileOutcome::Admitted),
            "{what}: the next batch still goes: {:?}",
            outcomes[B]
        );
        assert_eq!(
            submitted_ids(&fake),
            [vec![A], vec![B]],
            "{what}: not resubmitted"
        );
    }
}
