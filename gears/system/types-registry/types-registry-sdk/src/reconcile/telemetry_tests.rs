//! What reconciliation's tracing shows an investigator, captured under the filter a production
//! subscriber would use.

use std::collections::{BTreeMap, BTreeSet};
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use gts::GtsId;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use tracing::Level;
use tracing_subscriber::filter::LevelFilter;

use super::telemetry_capture::{Capture, Captured};
use super::{ReconcileOptions, ReconcileReport, ReconcileStop, Reconciliation, reconcile};
use crate::models::PublisherContext;
use crate::testing_platform::{Call, Fault, MockTypesRegistry};

const A: &str = "gts.cf.test.tel.a.v1~";
const B: &str = "gts.cf.test.tel.b.v1~";
const C: &str = "gts.cf.test.tel.c.v1~";
const BASE: &str = "gts.cf.test.tel.base.v1~";

/// Planted in documents and errors; never in any captured event.
const SENTINEL: &str = "SENTINEL-must-not-be-logged";

fn gid(id: &str) -> GtsId {
    GtsId::try_new(id).expect("a valid identifier")
}

fn doc(id: &str) -> (GtsId, Value) {
    (gid(id), json!({ "title": id, "secret": SENTINEL }))
}

fn publisher(name: &str) -> PublisherContext {
    PublisherContext {
        name: name.to_owned(),
        version: "3.1.4".parse().expect("version"),
    }
}

fn one_pass() -> ReconcileOptions {
    ReconcileOptions {
        passes: NonZeroU32::MIN,
        ..ReconcileOptions::default()
    }
}

async fn run(
    fake: &MockTypesRegistry,
    desired: &[(GtsId, Value)],
    options: &ReconcileOptions,
) -> Result<Reconciliation, CanonicalError> {
    reconcile(
        fake,
        &PlatformSecurityContext::outbound_marker(),
        &publisher("telemetry-test"),
        desired,
        options,
        &CancellationToken::new(),
    )
    .await
}

async fn run_as(
    fake: &MockTypesRegistry,
    name: &str,
    desired: Vec<(GtsId, Value)>,
    options: &ReconcileOptions,
) -> Result<Reconciliation, CanonicalError> {
    reconcile(
        fake,
        &PlatformSecurityContext::outbound_marker(),
        &publisher(name),
        &desired,
        options,
        &CancellationToken::new(),
    )
    .await
}

/// Refuse every registration carrying `id`, naming it, with a description quoting a value.
fn refuse_candidate(fake: &MockTypesRegistry, id: &str) {
    fake.inject(
        Fault::on(Call::Register).key(gid(id)).fail(
            crate::gts::TypeResource::invalid_argument()
                .with_resource(id.to_owned())
                .with_field_violation(
                    crate::field::ENTITY_FIELD,
                    format!("the value {SENTINEL} is not allowed"),
                    crate::field::VALIDATION_FAILED,
                )
                .create(),
        ),
    );
}

/// Extract the stop reason from an unsettled call.
fn unsettled(result: Result<Reconciliation, CanonicalError>) -> ReconcileStop {
    match result {
        Err(error) => {
            ReconcileReport::from_canonical(&error)
                .expect("report")
                .stop
        }
        other => panic!("expected an unsettled report, got {other:?}"),
    }
}

fn summary(capture: &Capture) -> Captured {
    capture.one("reconcile_finished")
}

fn assert_counts(summary: &Captured, expected: &[(&str, &str)]) {
    for (field, value) in expected {
        assert_eq!(summary.field(field), Some(*value), "{field}: {summary:#?}");
    }
}

fn assert_nothing_leaks(capture: &Capture) {
    for event in capture.events() {
        assert!(!event.text().contains(SENTINEL), "{event:#?}");
    }
}

/// Every event of the call is in one `reconcile` span; answers its id.
fn one_reconcile_id(capture: &Capture) -> String {
    let ids: BTreeSet<String> = capture
        .events()
        .iter()
        .map(|e| {
            e.span_field("reconcile", "reconcile_id")
                .unwrap_or_else(|| panic!("outside the reconcile span: {e:#?}"))
                .to_owned()
        })
        .collect();
    assert_eq!(ids.len(), 1, "{ids:?}");
    ids.into_iter().next().expect("one id")
}

#[tokio::test(start_paused = true)]
async fn an_up_to_date_call_reports_one_quiet_summary_in_its_span() {
    let fake = MockTypesRegistry::new();
    let (id, content) = doc(A);
    fake.seed(id.id(), content.clone());
    let capture = Capture::install(LevelFilter::INFO);

    let result = run(&fake, &[(id, content)], &ReconcileOptions::default()).await;

    assert!(matches!(result, Ok(Reconciliation::UpToDate)));
    one_reconcile_id(&capture);
    let finished = summary(&capture);
    assert_eq!(finished.level, Level::INFO);
    assert_counts(
        &finished,
        &[
            ("stop", "up_to_date"),
            ("admitted", "1"),
            ("undecided", "0"),
        ],
    );
    assert_eq!(
        finished.span_field("reconcile", "publisher"),
        Some("telemetry-test")
    );
    assert_eq!(
        finished.span_field("reconcile", "publisher_version"),
        Some("3.1.4")
    );
    assert!(
        capture.events().iter().all(|e| e.level > Level::WARN),
        "{:#?}",
        capture.events()
    );
}

#[tokio::test(start_paused = true)]
async fn a_refused_candidate_is_one_warning_naming_its_batch_and_the_rest_rows_are_skipped() {
    let fake = MockTypesRegistry::new();
    refuse_candidate(&fake, B);
    let capture = Capture::install(LevelFilter::INFO);

    let result = run(
        &fake,
        &[doc(A), doc(B), doc(C)],
        &ReconcileOptions::default(),
    )
    .await;

    assert_eq!(unsettled(result), ReconcileStop::InvalidInput);
    one_reconcile_id(&capture);
    let keys: Vec<String> = fake
        .submissions()
        .iter()
        .map(|(key, _)| key.as_str().to_owned())
        .collect();
    assert_eq!(keys.len(), 1, "nothing is resubmitted");

    let refused = capture.one("candidate_refused");
    assert_eq!(refused.level, Level::WARN);
    assert_eq!(refused.field("gts_id"), Some(B));
    assert_eq!(refused.field("remaining"), Some("2"));
    assert_eq!(refused.field("category"), Some("invalid_argument"));
    assert_eq!(refused.field("reason"), Some("VALIDATION_FAILED"));
    assert_eq!(refused.field("violation_fields"), Some("entity"));
    assert_eq!(refused.span_field("reconcile_batch", "batch"), Some("1"));
    assert_eq!(
        refused.span_field("reconcile_batch", "idempotency_key"),
        Some(keys[0].as_str())
    );

    let finished = summary(&capture);
    assert_eq!(finished.level, Level::WARN);
    assert_counts(
        &finished,
        &[
            ("stop", "invalid_input"),
            ("admitted", "0"),
            ("rejected", "1"),
            ("pending", "2"),
            ("undecided", "0"),
        ],
    );
    let rows = capture.named("entity_unsettled");
    let skipped: BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.field("cause") == Some("skipped"))
        .map(|r| {
            assert_eq!(r.field("category"), None, "no error is invented: {r:#?}");
            r.field("gts_id").expect("names the identifier")
        })
        .collect();
    assert_eq!(skipped, BTreeSet::from([A, C]));
    assert_nothing_leaks(&capture);
}

#[tokio::test(start_paused = true)]
async fn an_oversized_batch_reports_one_validation_refusal() {
    let fake = MockTypesRegistry::new().with_max_batch(2);
    let capture = Capture::install(LevelFilter::INFO);

    let result = run(
        &fake,
        &[doc(A), doc(B), doc(C)],
        &ReconcileOptions::default(),
    )
    .await;

    assert_eq!(unsettled(result), ReconcileStop::InvalidInput);
    let refused = capture.one("batch_refused");
    assert_eq!(refused.level, Level::WARN);
    assert_eq!(refused.span_field("reconcile_batch", "batch"), Some("1"));
    assert_eq!(capture.named("batch_submitted").len(), 1);
    assert_eq!(fake.calls(Call::Register), 1);
    assert_eq!(fake.calls(Call::GetOperation), 0);
    assert_eq!(summary(&capture).field("stop"), Some("invalid_input"));
}

#[tokio::test(start_paused = true)]
async fn a_transport_retry_keeps_the_key_and_logs_codes_only() {
    let fake = MockTypesRegistry::new();
    fake.inject(
        Fault::on(Call::Register)
            .first(1)
            .fail(CanonicalError::internal(SENTINEL).create()),
    );
    let capture = Capture::install(LevelFilter::DEBUG);

    run(&fake, &[doc(A)], &ReconcileOptions::default())
        .await
        .expect("reconciles");

    let retry = capture.one("transport_retry");
    assert_eq!(retry.field("category"), Some("internal"));
    assert_eq!(retry.span_field("reconcile_batch", "attempt"), Some("1"));
    let accepted = capture.one("batch_accepted");
    assert_eq!(accepted.span_field("reconcile_batch", "attempt"), Some("2"));
    assert_eq!(
        retry.span_field("reconcile_batch", "idempotency_key"),
        accepted.span_field("reconcile_batch", "idempotency_key"),
        "a transport retry reuses the key"
    );
    assert_eq!(summary(&capture).level, Level::INFO);
    assert_nothing_leaks(&capture);
}

#[tokio::test(start_paused = true)]
async fn a_refusal_of_the_whole_request_is_one_warning_and_pending_rows() {
    let fake = MockTypesRegistry::new();
    fake.inject(
        Fault::on(Call::Register).fail(
            crate::gts::TypeResource::invalid_argument()
                .with_field_violation("publisher", SENTINEL, "VALIDATION_FAILED")
                .create(),
        ),
    );
    let capture = Capture::install(LevelFilter::INFO);

    assert_eq!(
        unsettled(run(&fake, &[doc(A), doc(B)], &ReconcileOptions::default()).await),
        ReconcileStop::InvalidInput
    );

    let refused = capture.one("batch_refused");
    assert_eq!(refused.level, Level::WARN);
    assert_eq!(refused.field("violation_fields"), Some("publisher"));
    assert_eq!(refused.span_field("reconcile_batch", "size"), Some("2"));
    let finished = summary(&capture);
    assert_eq!(finished.level, Level::WARN);
    assert_counts(
        &finished,
        &[
            ("stop", "invalid_input"),
            ("pending", "2"),
            ("admitted", "0"),
        ],
    );
    let rows = capture.named("entity_unsettled");
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r.field("outcome") == Some("pending") && r.field("cause") == Some("refused"))
    );
    assert_nothing_leaks(&capture);
}

#[tokio::test(start_paused = true)]
async fn a_deadline_passes_exhaustion_and_a_read_failure_are_distinct_stops() {
    // Deadline: the submission never answers within the budget.
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::on(Call::Register).delay(Duration::from_secs(3600)));
    let capture = Capture::install(LevelFilter::INFO);
    let options = ReconcileOptions {
        deadline: Duration::from_secs(10),
        ..ReconcileOptions::default()
    };
    assert_eq!(
        unsettled(run(&fake, &[doc(A)], &options).await),
        ReconcileStop::Deadline
    );
    assert_counts(
        &summary(&capture),
        &[("stop", "deadline"), ("pending", "1")],
    );
    drop(capture);

    // Passes: a dependency that never arrives.
    let fake = MockTypesRegistry::new();
    fake.depends_on(A, BASE);
    let capture = Capture::install(LevelFilter::INFO);
    let options = ReconcileOptions {
        passes: NonZeroU32::new(2).expect("two"),
        ..ReconcileOptions::default()
    };
    assert_eq!(
        unsettled(run(&fake, &[doc(A)], &options).await),
        ReconcileStop::PassesExhausted
    );
    assert_counts(
        &summary(&capture),
        &[
            ("stop", "passes_exhausted"),
            ("pending", "1"),
            ("passes", "2"),
        ],
    );
    let row = capture.one("entity_unsettled");
    assert_eq!(row.field("cause"), Some("dependency"));
    assert_eq!(row.field("reason"), Some("dependency_not_found"));
    assert_eq!(row.field("dependency_id"), Some(BASE));
    drop(capture);

    // Read failure.
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::on(Call::BatchGet).fail(CanonicalError::internal(SENTINEL).create()));
    let capture = Capture::install(LevelFilter::INFO);
    assert_eq!(
        unsettled(run(&fake, &[doc(A)], &one_pass()).await),
        ReconcileStop::ReadFailed
    );
    let finished = summary(&capture);
    assert_eq!(finished.level, Level::WARN);
    assert_counts(&finished, &[("stop", "read_failed"), ("pending", "1")]);
    assert_nothing_leaks(&capture);
}

#[tokio::test(start_paused = true)]
async fn an_unrepresentable_deadline_still_reports_its_end() {
    let fake = MockTypesRegistry::new();
    let capture = Capture::install(LevelFilter::INFO);
    let options = ReconcileOptions {
        deadline: Duration::MAX,
        ..ReconcileOptions::default()
    };

    run(&fake, &[doc(A)], &options).await.expect_err("refused");

    let finished = summary(&capture);
    assert_eq!(finished.level, Level::WARN);
    assert_counts(
        &finished,
        &[
            ("stop", "invalid_deadline"),
            ("desired", "1"),
            ("undecided", "1"),
        ],
    );
}

#[tokio::test(start_paused = true)]
async fn duplicates_count_once_and_leave_nothing_undecided() {
    let fake = MockTypesRegistry::new();
    let capture = Capture::install(LevelFilter::INFO);
    run(&fake, &[doc(A), doc(A)], &ReconcileOptions::default())
        .await
        .expect("reconciles");
    let identical = summary(&capture);
    assert_eq!(identical.span_field("reconcile", "inputs"), Some("2"));
    assert_eq!(identical.span_field("reconcile", "desired"), Some("1"));
    assert_counts(&identical, &[("admitted", "1"), ("undecided", "0")]);
    drop(capture);

    let fake = MockTypesRegistry::new();
    let capture = Capture::install(LevelFilter::INFO);
    unsettled(
        run(
            &fake,
            &[(gid(A), json!({ "one": 1 })), (gid(A), json!({ "two": 2 }))],
            &ReconcileOptions::default(),
        )
        .await,
    );
    let conflicting = summary(&capture);
    assert_eq!(conflicting.span_field("reconcile", "desired"), Some("1"));
    assert_counts(&conflicting, &[("rejected", "1"), ("undecided", "0")]);
}

/// A, then B refused by admission, each in its own batch; C's submission never answers.
fn partially_decided(fake: &MockTypesRegistry) -> ReconcileOptions {
    fake.reject(
        B,
        crate::item_failure::AdmissionFailureReason::InvalidSchema,
    );
    fake.inject(
        Fault::on(Call::Register)
            .key(gid(C))
            .delay(Duration::from_secs(3600)),
    );
    ReconcileOptions {
        batch_size: NonZeroUsize::MIN,
        deadline: Duration::from_secs(36_000),
        ..ReconcileOptions::default()
    }
}

#[tokio::test(start_paused = true)]
async fn a_dropped_call_reports_what_it_had_observed() {
    let fake = MockTypesRegistry::new();
    let options = partially_decided(&fake);
    let capture = Capture::install(LevelFilter::INFO);
    let desired = [doc(A), doc(B), doc(C)];
    let mut call = Box::pin(run(&fake, &desired, &options));

    let waited = tokio::time::timeout(Duration::from_secs(5), &mut call).await;
    assert!(waited.is_err(), "C's submission is still waiting");
    assert!(capture.named("reconcile_finished").is_empty());
    drop(call);

    let finished = summary(&capture);
    assert_eq!(finished.level, Level::WARN);
    assert_counts(
        &finished,
        &[
            ("stop", "dropped"),
            ("admitted", "1"),
            ("rejected", "1"),
            ("undecided", "1"),
        ],
    );
    assert!(finished.span_field("reconcile", "reconcile_id").is_some());
    assert!(
        capture.named("entity_unsettled").is_empty(),
        "no rows for an interrupted call"
    );
}

#[tokio::test(start_paused = true)]
async fn cancelling_by_token_is_quiet_and_unlike_a_registry_cancellation() {
    let fake = Arc::new(MockTypesRegistry::new());
    let options = partially_decided(&fake);
    let capture = Capture::install(LevelFilter::INFO);
    let cancel = CancellationToken::new();
    let task = {
        let (fake, cancel) = (Arc::clone(&fake), cancel.clone());
        tokio::spawn(async move {
            let desired = [doc(A), doc(B), doc(C)];
            reconcile(
                &*fake,
                &PlatformSecurityContext::outbound_marker(),
                &publisher("telemetry-test"),
                &desired,
                &options,
                &cancel,
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_secs(5)).await;
    cancel.cancel();
    let result = task.await.expect("joins");

    assert!(matches!(result, Err(CanonicalError::Cancelled { .. })));
    let finished = summary(&capture);
    assert_eq!(finished.level, Level::INFO);
    assert_counts(
        &finished,
        &[
            ("stop", "cancelled"),
            ("admitted", "1"),
            ("rejected", "1"),
            ("undecided", "1"),
        ],
    );
    drop(capture);

    // The registry's own `Cancelled` is an answer: the call ends normally, the row says so.
    let fake = MockTypesRegistry::new();
    fake.inject(
        Fault::on(Call::BatchGet).fail(crate::gts::OperationResource::cancelled().create()),
    );
    let capture = Capture::install(LevelFilter::INFO);
    unsettled(run(&fake, &[doc(A)], &one_pass()).await);
    assert_counts(&summary(&capture), &[("stop", "read_failed")]);
    let row = capture.one("entity_unsettled");
    assert_eq!(row.field("cause"), Some("refused"));
    assert_eq!(row.field("category"), Some("cancelled"));
}

#[tokio::test(start_paused = true)]
async fn interleaved_calls_keep_their_own_ids() {
    let fake = MockTypesRegistry::new().completing_after(2);
    let capture = Capture::install(LevelFilter::INFO);
    let options = ReconcileOptions {
        batch_size: NonZeroUsize::MIN,
        ..ReconcileOptions::default()
    };
    let (one, two) = tokio::join!(
        run_as(&fake, "first", vec![doc(A), doc(B)], &options),
        run_as(&fake, "second", vec![doc(C), doc(BASE)], &options)
    );
    one.expect("first reconciles");
    two.expect("second reconciles");

    let mut run_of_key: BTreeMap<String, String> = BTreeMap::new();
    let mut ids_by_publisher: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for event in capture.events() {
        let id = event
            .span_field("reconcile", "reconcile_id")
            .expect("in a reconcile span")
            .to_owned();
        let publisher = event
            .span_field("reconcile", "publisher")
            .expect("publisher")
            .to_owned();
        ids_by_publisher
            .entry(publisher)
            .or_default()
            .insert(id.clone());
        if let Some(key) = event.span_field("reconcile_batch", "idempotency_key") {
            let previous = run_of_key.insert(key.to_owned(), id.clone());
            assert!(
                previous.is_none_or(|p| p == id),
                "a key belongs to one call"
            );
        }
    }
    assert_eq!(ids_by_publisher.len(), 2);
    assert!(
        ids_by_publisher.values().all(|ids| ids.len() == 1),
        "{ids_by_publisher:?}"
    );
    assert_ne!(ids_by_publisher["first"], ids_by_publisher["second"]);
    assert_eq!(
        run_of_key.len(),
        4,
        "every batch of both calls is correlated"
    );
}

#[tokio::test(start_paused = true)]
async fn a_policy_refusal_is_explained_by_its_codes() {
    for parameter in [
        crate::precondition::REGISTRATION_POLICY_ALLOWED_VENDORS,
        crate::precondition::REGISTRATION_POLICY_TENANT_OWNABLE,
    ] {
        let fake = MockTypesRegistry::new();
        // The registry's policy refusal shape: the candidate as resource, the region as subject.
        fake.inject(
            Fault::on(Call::Register).fail(
                crate::gts::TypeResource::failed_precondition()
                    .with_resource(A.to_owned())
                    .with_precondition_violation("<default>", SENTINEL, parameter)
                    .create(),
            ),
        );
        let capture = Capture::install(LevelFilter::INFO);

        unsettled(run(&fake, &[doc(A)], &one_pass()).await);

        let row = capture.one("entity_unsettled");
        assert_eq!(row.field("category"), Some("failed_precondition"));
        assert_eq!(row.field("reason"), Some(parameter), "{row:#?}");
        assert_nothing_leaks(&capture);
    }
}

#[tokio::test(start_paused = true)]
async fn a_deadline_spent_in_the_last_pass_is_the_stop() {
    let fake = MockTypesRegistry::new();
    fake.inject(Fault::on(Call::Register).delay(Duration::from_secs(3600)));
    let capture = Capture::install(LevelFilter::INFO);
    let options = ReconcileOptions {
        deadline: Duration::from_secs(10),
        ..one_pass()
    };

    unsettled(run(&fake, &[doc(A)], &options).await);

    assert_counts(
        &summary(&capture),
        &[("stop", "deadline"), ("pending", "1"), ("passes", "1")],
    );
    // No acknowledgement arrived, yet the INFO log names the key the registry may hold.
    assert!(capture.named("batch_accepted").is_empty());
    assert_eq!(fake.calls(Call::Register), 1);
    let submitted = capture.one("batch_submitted");
    let key = submitted
        .span_field("reconcile_batch", "idempotency_key")
        .expect("the key is named before any answer");
    assert!(uuid::Uuid::try_parse(key).is_ok(), "{key}");
}

#[tokio::test(start_paused = true)]
async fn a_registry_deadline_within_the_budget_is_not_the_calls_deadline() {
    let fake = MockTypesRegistry::new();
    fake.inject(
        Fault::on(Call::Register).fail(
            crate::gts::OperationResource::deadline_exceeded("the registry timed out").create(),
        ),
    );
    let capture = Capture::install(LevelFilter::INFO);

    unsettled(run(&fake, &[doc(A)], &one_pass()).await);

    let finished = summary(&capture);
    assert_counts(&finished, &[("stop", "passes_exhausted"), ("pending", "1")]);
    assert_eq!(
        capture.one("entity_unsettled").field("category"),
        Some("deadline_exceeded")
    );
}
