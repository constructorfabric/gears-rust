use std::collections::BTreeMap;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use super::{Liveness, Outcome, PendingCause, ReconcileOptions, Reconciliation, reconcile};
use crate::item_failure::AdmissionFailure;
use crate::models::{IdempotencyKey, PublisherContext, RegisterEntitiesRequest};
use crate::testing_platform::{FakePlatformRegistry, ReadFault};

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

fn doc(id: &str) -> (String, Value) {
    (id.to_owned(), json!({ "title": id }))
}

async fn run(fake: &FakePlatformRegistry, desired: Vec<(String, Value)>) -> Reconciliation {
    run_with(fake, desired, &options()).await
}

async fn run_with(
    fake: &FakePlatformRegistry,
    desired: Vec<(String, Value)>,
    options: &ReconcileOptions,
) -> Reconciliation {
    reconcile(
        fake,
        &ctx(),
        &publisher(),
        &desired,
        options,
        &CancellationToken::new(),
    )
    .await
    .expect("reconciles")
}

fn outcomes(result: Reconciliation) -> BTreeMap<String, Outcome> {
    match result {
        Reconciliation::Reconciled(outcomes) => outcomes,
        Reconciliation::UpToDate => panic!("expected outcomes, got UpToDate"),
    }
}

fn reason(outcome: &Outcome) -> String {
    let error = match outcome {
        Outcome::Rejected(e)
        | Outcome::Superseded { error: e, .. }
        | Outcome::Pending(
            PendingCause::Dependency(e)
            | PendingCause::Conflict(e)
            | PendingCause::Unavailable(e)
            | PendingCause::Refused(e),
        ) => e,
        Outcome::Admitted => return "admitted".to_owned(),
    };
    AdmissionFailure::from_canonical(error)
        .map_or_else(|| format!("{error:?}"), |f| f.reason.as_wire().to_owned())
}

fn keys(submissions: &[(IdempotencyKey, RegisterEntitiesRequest)]) -> Vec<String> {
    submissions
        .iter()
        .map(|(k, _)| k.as_str().to_owned())
        .collect()
}

#[tokio::test]
async fn everything_already_matching_is_up_to_date_without_a_submission() {
    let fake = FakePlatformRegistry::new();
    let (id, content) = doc(A);
    fake.seed(&id, content.clone());

    let result = run(&fake, vec![(id, content)]).await;

    assert!(matches!(result, Reconciliation::UpToDate), "{result:?}");
    assert!(fake.submissions().is_empty());
}

#[tokio::test]
async fn only_supplied_documents_are_reconciled() {
    let fake = FakePlatformRegistry::new();
    fake.seed(B, json!({ "unrelated": true }));

    let outcomes = outcomes(run(&fake, vec![doc(A)]).await);

    assert!(matches!(outcomes[A], Outcome::Admitted));
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
    let fake = FakePlatformRegistry::new();
    fake.seed(A, json!({ "old": true }));

    let outcomes = outcomes(run(&fake, vec![doc(A)]).await);

    assert!(matches!(outcomes[A], Outcome::Admitted));
    let submissions = fake.submissions();
    assert_eq!(submissions[0].1.items[0].expected_resource_version, Some(1));
    assert_eq!(submissions[0].1.publisher, publisher());
}

#[tokio::test(start_paused = true)]
async fn a_dependency_published_later_is_picked_up_on_the_next_pass_under_a_new_key() {
    // Decided at submit, so the base is seeded only after the first outcome.
    let fake = Arc::new(FakePlatformRegistry::new().completing_after(0));
    let desired = vec![(A.to_owned(), json!({ "x-fake-depends-on": BASE }))];
    let task = {
        let fake = Arc::clone(&fake);
        tokio::spawn(async move { run(&fake, desired).await })
    };
    while fake.submissions().is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    fake.seed(BASE, json!({}));

    let outcomes = outcomes(task.await.expect("joins"));
    assert!(
        matches!(outcomes[A], Outcome::Admitted),
        "{:?}",
        outcomes[A]
    );
    let keys = keys(&fake.submissions());
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1], "a new pass takes a new key");
}

#[tokio::test(start_paused = true)]
async fn a_concurrent_publisher_of_identical_content_ends_admitted_through_a_re_read() {
    let fake = FakePlatformRegistry::new();
    let (id, content) = doc(A);
    fake.race_next_submit(&id, content.clone());

    let outcomes = outcomes(run(&fake, vec![(id, content)]).await);

    assert!(
        matches!(outcomes[A], Outcome::Admitted),
        "{:?}",
        outcomes[A]
    );
    assert_eq!(fake.submissions().len(), 1, "the re-read found it equal");
}

#[tokio::test(start_paused = true)]
async fn two_publishers_of_identical_content_both_end_admitted() {
    let fake = Arc::new(FakePlatformRegistry::new());

    let (first, second) = tokio::join!(run(&fake, vec![doc(A)]), run(&fake, vec![doc(A)]));

    for result in [first, second] {
        match result {
            Reconciliation::UpToDate => {}
            Reconciliation::Reconciled(outcomes) => {
                assert!(
                    matches!(outcomes[A], Outcome::Admitted),
                    "{:?}",
                    outcomes[A]
                );
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_conflict_re_reads_and_retries_with_the_new_precondition_and_a_new_key() {
    let fake = FakePlatformRegistry::new();
    fake.seed(A, json!({ "v": 1 }));
    fake.race_next_submit(A, json!({ "v": "someone else" }));

    let outcomes = outcomes(run(&fake, vec![doc(A)]).await);

    assert!(
        matches!(outcomes[A], Outcome::Admitted),
        "{:?}",
        outcomes[A]
    );
    let submissions = fake.submissions();
    assert_eq!(submissions.len(), 2);
    assert_eq!(submissions[0].1.items[0].expected_resource_version, Some(1));
    assert_eq!(submissions[1].1.items[0].expected_resource_version, Some(2));
    assert_ne!(submissions[0].0, submissions[1].0);
}

#[tokio::test(start_paused = true)]
async fn a_lost_receipt_is_recovered_under_the_same_key_and_request() {
    let fake = FakePlatformRegistry::new();
    fake.lose_read_backs(1);

    let outcomes = outcomes(run(&fake, vec![doc(A)]).await);

    assert!(
        matches!(outcomes[A], Outcome::Admitted),
        "{:?}",
        outcomes[A]
    );
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
    let fake = FakePlatformRegistry::new();

    let outcomes = outcomes(
        run(
            &fake,
            vec![(A.to_owned(), json!({ "x-fake-invalid": true }))],
        )
        .await,
    );

    assert!(matches!(outcomes[A], Outcome::Rejected(_)));
    assert_eq!(reason(&outcomes[A]), "invalid_schema");
    assert_eq!(fake.submissions().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_synchronously_refused_batch_is_bisected_and_every_payload_has_its_own_key() {
    let fake = FakePlatformRegistry::new().with_max_batch(2);
    let desired: Vec<_> = (0..5)
        .map(|n| doc(&format!("gts.cf.test.pkg.n{n}.v1~")))
        .collect();

    let outcomes = outcomes(run(&fake, desired).await);

    assert_eq!(outcomes.len(), 5);
    assert!(outcomes.values().all(|o| matches!(o, Outcome::Admitted)));
    let mut by_key: BTreeMap<String, Vec<RegisterEntitiesRequest>> = BTreeMap::new();
    for (key, request) in fake.submissions() {
        by_key
            .entry(key.as_str().to_owned())
            .or_default()
            .push(request);
    }
    for requests in by_key.values() {
        assert!(
            requests.windows(2).all(|w| w[0] == w[1]),
            "a key is never reused for a different payload"
        );
    }
}

#[tokio::test]
async fn candidates_go_out_in_batches_of_the_configured_size() {
    let fake = FakePlatformRegistry::new();
    let desired: Vec<_> = (0..5)
        .map(|n| doc(&format!("gts.cf.test.pkg.n{n}.v1~")))
        .collect();
    let options = ReconcileOptions {
        batch_size: NonZeroUsize::new(2).expect("non-zero"),
        ..options()
    };

    outcomes(run_with(&fake, desired, &options).await);

    let submissions = fake.submissions();
    let sizes: Vec<usize> = submissions.iter().map(|(_, r)| r.items.len()).collect();
    assert_eq!(sizes, [2, 2, 1]);
    let mut keys = keys(&submissions);
    keys.dedup();
    assert_eq!(keys.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn later_batches_proceed_when_an_earlier_one_waits_on_a_dependency() {
    let fake = FakePlatformRegistry::new();
    let options = ReconcileOptions {
        batch_size: NonZeroUsize::new(1).expect("non-zero"),
        passes: NonZeroU32::new(2).expect("non-zero"),
        ..options()
    };

    let outcomes = outcomes(
        run_with(
            &fake,
            vec![(A.to_owned(), json!({ "x-fake-depends-on": BASE })), doc(B)],
            &options,
        )
        .await,
    );

    assert!(matches!(outcomes[B], Outcome::Admitted));
    assert!(matches!(
        outcomes[A],
        Outcome::Pending(PendingCause::Dependency(_))
    ));
    assert_eq!(reason(&outcomes[A]), "dependency_not_found");
}

#[tokio::test]
async fn invalid_input_alone_is_reported_not_up_to_date() {
    let fake = FakePlatformRegistry::new();

    let outcomes = outcomes(run(&fake, vec![("not-an-id".to_owned(), json!({}))]).await);

    assert!(matches!(outcomes["not-an-id"], Outcome::Rejected(_)));
    assert!(fake.submissions().is_empty());
}

#[tokio::test]
async fn an_equal_document_beside_a_rejected_one_is_reported_with_it() {
    let fake = FakePlatformRegistry::new();
    let (id, content) = doc(A);
    fake.seed(&id, content.clone());

    let outcomes = outcomes(
        run(
            &fake,
            vec![(id, content), ("not-an-id".to_owned(), json!({}))],
        )
        .await,
    );

    assert!(matches!(outcomes[A], Outcome::Admitted));
    assert!(matches!(outcomes["not-an-id"], Outcome::Rejected(_)));
}

#[tokio::test]
async fn an_identifier_declared_twice_differently_is_rejected_and_identical_twins_collapse() {
    let fake = FakePlatformRegistry::new();

    let outcomes = outcomes(
        run(
            &fake,
            vec![
                (A.to_owned(), json!({ "one": 1 })),
                (A.to_owned(), json!({ "two": 2 })),
                doc(B),
                doc(B),
            ],
        )
        .await,
    );

    assert!(matches!(outcomes[A], Outcome::Rejected(_)));
    assert!(matches!(outcomes[B], Outcome::Admitted));
    let submitted: usize = fake.submissions().iter().map(|(_, r)| r.items.len()).sum();
    assert_eq!(submitted, 1);
}

#[tokio::test]
async fn nothing_desired_is_up_to_date() {
    let fake = FakePlatformRegistry::new();
    assert!(matches!(
        run(&fake, Vec::new()).await,
        Reconciliation::UpToDate
    ));
}

#[tokio::test(start_paused = true)]
async fn superseded_liveness_comes_from_a_read_after_the_outcome() {
    let fake = FakePlatformRegistry::new();
    fake.seed(A, json!({ "newer": true }));
    fake.seed(B, json!({ "newer": true }));

    let outcomes = outcomes(
        run(
            &fake,
            vec![
                (A.to_owned(), json!({ "x-fake-superseded": true })),
                (
                    B.to_owned(),
                    json!({ "x-fake-superseded": true, "x-fake-superseded-deletes": true }),
                ),
            ],
        )
        .await,
    );

    assert!(matches!(
        outcomes[A],
        Outcome::Superseded {
            liveness: Liveness::Live,
            ..
        }
    ));
    assert!(
        matches!(
            outcomes[B],
            Outcome::Superseded {
                liveness: Liveness::Deleted,
                ..
            }
        ),
        "a deletion committed with the outcome is seen: {:?}",
        outcomes[B]
    );
    assert_eq!(fake.submissions().len(), 1, "never resubmitted");
}

#[tokio::test]
async fn an_unrepresentable_deadline_is_refused() {
    let fake = FakePlatformRegistry::new();
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

    assert!(matches!(error, CanonicalError::InvalidArgument { .. }));
    assert!(fake.submissions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_unreachable_registry_leaves_identifiers_pending_unavailable() {
    let fake = FakePlatformRegistry::new();
    fake.delay_submits(Duration::from_secs(3600));
    let options = ReconcileOptions {
        deadline: Duration::from_secs(5),
        ..options()
    };

    let outcomes = outcomes(run_with(&fake, vec![doc(A)], &options).await);

    assert!(matches!(
        outcomes[A],
        Outcome::Pending(PendingCause::Unavailable(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn an_equal_identifier_settles_at_once_and_a_later_read_failure_cannot_downgrade_it() {
    let fake = FakePlatformRegistry::new();
    let (a, content) = doc(A);
    fake.seed(&a, content.clone());
    fake.fault_reads(ReadFault::FailFrom(2));

    let outcomes = outcomes(
        run(
            &fake,
            vec![
                (a, content),
                (B.to_owned(), json!({ "x-fake-depends-on": BASE })),
            ],
        )
        .await,
    );

    assert!(
        matches!(outcomes[A], Outcome::Admitted),
        "{:?}",
        outcomes[A]
    );
    assert!(matches!(
        outcomes[B],
        Outcome::Pending(PendingCause::Unavailable(_))
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
    let fake = Arc::new(FakePlatformRegistry::new().completing_after(0));
    let (a, content) = doc(A);
    fake.seed(&a, content.clone());
    let desired = vec![
        (a, content),
        (B.to_owned(), json!({ "x-fake-depends-on": BASE })),
    ];
    let task = {
        let fake = Arc::clone(&fake);
        tokio::spawn(async move { run(&fake, desired).await })
    };
    while fake.submissions().is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    fake.seed(A, json!({ "changed": "by someone else" }));
    fake.seed(BASE, json!({}));

    let outcomes = outcomes(task.await.expect("joins"));
    assert!(matches!(outcomes[A], Outcome::Admitted));
    assert!(matches!(outcomes[B], Outcome::Admitted));
    assert!(
        fake.submissions()
            .iter()
            .all(|(_, r)| r.items.iter().all(|i| i.gts_id.to_string() == B))
    );
}

#[tokio::test]
async fn an_incomplete_read_submits_nothing() {
    for fault in [ReadFault::DropAnswers, ReadFault::StripOrigin] {
        let fake = FakePlatformRegistry::new();
        fake.seed(A, json!({ "old": true }));
        fake.fault_reads(fault);
        let options = ReconcileOptions {
            passes: NonZeroU32::new(1).expect("non-zero"),
            ..options()
        };

        let outcomes = outcomes(run_with(&fake, vec![doc(A)], &options).await);

        assert!(
            matches!(outcomes[A], Outcome::Pending(PendingCause::Unavailable(_))),
            "{fault:?}: {:?}",
            outcomes[A]
        );
        assert!(fake.submissions().is_empty(), "{fault:?}: no POST");
    }
}

#[tokio::test(start_paused = true)]
async fn another_publishers_entity_is_rejected_not_superseded() {
    let fake = FakePlatformRegistry::new();
    fake.seed(A, json!({ "theirs": true }));
    fake.seed(B, json!({ "newer": true }));

    let outcomes = outcomes(
        run(
            &fake,
            vec![
                (A.to_owned(), json!({ "x-fake-publisher-mismatch": true })),
                (B.to_owned(), json!({ "x-fake-superseded": true })),
            ],
        )
        .await,
    );

    assert!(
        matches!(outcomes[A], Outcome::Rejected(_)),
        "{:?}",
        outcomes[A]
    );
    assert_eq!(reason(&outcomes[A]), "publisher_mismatch");
    assert!(matches!(
        outcomes[B],
        Outcome::Superseded {
            liveness: Liveness::Live,
            ..
        }
    ));
}

#[tokio::test(start_paused = true)]
async fn an_oversized_backoff_is_capped_by_the_deadline() {
    let fake = FakePlatformRegistry::new();
    let options = ReconcileOptions {
        retry_backoff: Duration::MAX,
        retry_backoff_max: Duration::MAX,
        deadline: Duration::from_secs(5),
        ..options()
    };
    let started = tokio::time::Instant::now();

    let outcomes = outcomes(
        run_with(
            &fake,
            vec![(A.to_owned(), json!({ "x-fake-depends-on": BASE }))],
            &options,
        )
        .await,
    );

    assert!(started.elapsed() <= Duration::from_secs(5));
    assert!(
        matches!(outcomes[A], Outcome::Pending(_)),
        "{:?}",
        outcomes[A]
    );
}

#[tokio::test(start_paused = true)]
async fn a_completed_operation_reporting_no_items_keeps_every_identifier_pending() {
    let fake = FakePlatformRegistry::new();
    fake.drop_operation_items();
    // One pass: a second would re-read and legitimately find the writes applied.
    let options = ReconcileOptions {
        passes: NonZeroU32::new(1).expect("non-zero"),
        ..options()
    };

    let outcomes = outcomes(run_with(&fake, vec![doc(A), doc(B)], &options).await);

    assert_eq!(outcomes.len(), 2, "every desired identifier is retained");
    assert!(
        outcomes
            .values()
            .all(|o| matches!(o, Outcome::Pending(PendingCause::Unavailable(_)))),
        "{outcomes:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn no_batch_is_posted_once_the_budget_is_spent() {
    let fake = FakePlatformRegistry::new();
    fake.delay_submits(Duration::from_secs(3));
    let options = ReconcileOptions {
        batch_size: NonZeroUsize::new(1).expect("non-zero"),
        deadline: Duration::from_secs(5),
        ..options()
    };

    let outcomes = outcomes(run_with(&fake, vec![doc(A), doc(B), doc(C)], &options).await);

    assert_eq!(
        fake.submissions().len(),
        1,
        "the second outlives the budget, the third never starts"
    );
    assert!(matches!(outcomes[A], Outcome::Admitted));
    for id in [B, C] {
        assert!(matches!(
            outcomes[id],
            Outcome::Pending(PendingCause::Unavailable(_))
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn a_zero_budget_reconciliation_submits_nothing() {
    let fake = FakePlatformRegistry::new();
    let options = ReconcileOptions {
        deadline: Duration::ZERO,
        ..options()
    };

    let outcomes = outcomes(run_with(&fake, vec![doc(A)], &options).await);

    assert!(matches!(
        outcomes[A],
        Outcome::Pending(PendingCause::Unavailable(_))
    ));
    assert!(fake.submissions().is_empty());
    assert_eq!(fake.batch_reads(), 0);
}

fn one_pass() -> ReconcileOptions {
    ReconcileOptions {
        passes: NonZeroU32::MIN,
        ..options()
    }
}

#[tokio::test(start_paused = true)]
async fn transport_retries_stop_at_their_bound_under_one_key() {
    let fake = FakePlatformRegistry::new();
    fake.lose_read_backs(5);

    let outcomes = outcomes(run_with(&fake, vec![doc(A)], &one_pass()).await);

    assert!(
        matches!(outcomes[A], Outcome::Pending(PendingCause::Unavailable(_))),
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
    let fake = FakePlatformRegistry::new();
    fake.fail_submits(2, &CanonicalError::internal("transient").create());

    let outcomes = outcomes(run_with(&fake, vec![doc(A)], &one_pass()).await);

    assert!(
        matches!(outcomes[A], Outcome::Admitted),
        "{:?}",
        outcomes[A]
    );
    let submissions = fake.submissions();
    assert_eq!(submissions.len(), 3);
    assert!(submissions.iter().all(|(k, _)| *k == submissions[0].0));
}

#[tokio::test(start_paused = true)]
async fn a_refusal_of_the_call_is_submitted_once_and_stays_pending_as_refused() {
    let fake = FakePlatformRegistry::new();
    fake.fail_submits(
        1,
        &CanonicalError::unauthenticated()
            .with_reason("TOKEN_INVALID")
            .create(),
    );

    let outcomes = outcomes(run_with(&fake, vec![doc(A)], &one_pass()).await);

    assert!(
        matches!(
            outcomes[A],
            Outcome::Pending(PendingCause::Refused(
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
    use crate::models::CandidateStatus;

    let failed = |r: &str| {
        classify(
            CandidateStatus::Failed,
            Some(
                AdmissionFailure::new(
                    crate::item_failure::AdmissionFailureReason::from_wire(r),
                    "m",
                )
                .into_canonical(A),
            ),
        )
    };
    for r in [
        reason::DEPENDENCY_NOT_FOUND,
        reason::BLOCKED_BY_DEPENDENCY,
        reason::BLOCKED_BY_PREDECESSOR,
        reason::MISSING_PREDECESSOR,
    ] {
        assert!(
            matches!(failed(r), Outcome::Pending(PendingCause::Dependency(_))),
            "{r}"
        );
    }
    for r in [reason::ALREADY_EXISTS, reason::PRECONDITION_FAILED] {
        assert!(
            matches!(failed(r), Outcome::Pending(PendingCause::Conflict(_))),
            "{r}"
        );
    }
    assert!(matches!(
        failed(reason::SYSTEM_FAILURE),
        Outcome::Pending(PendingCause::Unavailable(_))
    ));
    assert!(matches!(
        failed(reason::SUPERSEDED),
        Outcome::Superseded {
            liveness: Liveness::Unverified,
            ..
        }
    ));
    for r in [
        reason::PUBLISHER_MISMATCH,
        "invalid_schema",
        "future_reason",
    ] {
        assert!(matches!(failed(r), Outcome::Rejected(_)), "{r}");
    }
}

#[test]
fn malformed_or_undecided_items_map_to_their_outcome() {
    use super::classify;
    use crate::models::CandidateStatus;

    assert!(matches!(
        classify(CandidateStatus::Failed, None),
        Outcome::Rejected(CanonicalError::Internal { .. })
    ));
    for status in [CandidateStatus::Pending, CandidateStatus::Running] {
        assert!(
            matches!(
                classify(status, None),
                Outcome::Pending(PendingCause::Unavailable(_))
            ),
            "{status:?}"
        );
    }
    for status in [CandidateStatus::Succeeded, CandidateStatus::Unchanged] {
        assert!(
            matches!(classify(status, None), Outcome::Admitted),
            "{status:?}"
        );
    }
}

/// Measure virtual pass time for an absent dependency; one read per pass.
async fn time_pending_passes(passes: u32, backoff: Duration, max: Duration) -> (Duration, u32) {
    // Completing on submit: no poll interval adds to the passes' pauses.
    let fake = FakePlatformRegistry::new().completing_after(0);
    let options = ReconcileOptions {
        passes: NonZeroU32::new(passes).expect("non-zero"),
        retry_backoff: backoff,
        retry_backoff_max: max,
        deadline: Duration::from_secs(600),
        ..options()
    };
    let started = tokio::time::Instant::now();
    let outcomes = outcomes(
        run_with(
            &fake,
            vec![(A.to_owned(), json!({ "x-fake-depends-on": BASE }))],
            &options,
        )
        .await,
    );
    assert!(matches!(
        outcomes[A],
        Outcome::Pending(PendingCause::Dependency(_))
    ));
    (started.elapsed(), fake.batch_reads())
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

#[tokio::test(start_paused = true)]
async fn a_superseded_entity_the_re_read_does_not_find_is_deleted() {
    let fake = FakePlatformRegistry::new();

    let outcomes = outcomes(
        run(
            &fake,
            vec![(A.to_owned(), json!({ "x-fake-superseded": true }))],
        )
        .await,
    );

    assert!(
        matches!(
            outcomes[A],
            Outcome::Superseded {
                liveness: Liveness::Deleted,
                ..
            }
        ),
        "a successful read that finds nothing is not unverified: {:?}",
        outcomes[A]
    );
    assert_eq!(fake.submissions().len(), 1, "never resubmitted");
}
