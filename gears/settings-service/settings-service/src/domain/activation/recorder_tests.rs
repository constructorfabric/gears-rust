//! The Activation Outcome Recorder over `SQLite`: the outcome rules, the
//! guarded transition, and the `activation` audit record written with it.
//!
//! Level 1 rows of DESIGN-activation §7 (group B): *Matching value succeeds*,
//! *Wrong value with the current `change_ref`*, *Stale report ignored*,
//! *Repeat report*, *No row*, *Activation audit record once per transition*,
//! *Outcome status — secret hash compare*, and the error rows of the report
//! flow. The concurrent rows run on `PostgreSQL`
//! (`tests/pg_activation_delivery_test.rs`).
//!
//! Log-capture assertions assume one process per test (nextest, as the gear's
//! `make` targets run) and may flake under plain `cargo test`.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::audit::{
    ActorClassification, AuditOperation, AuditOutcome, AuditRecord, AuditSink, StoredAuditRecord,
};
use crate::domain::activation::publisher::{StateRow, StateWrite};
use crate::domain::activation::recorder::{
    DETAIL_LIMIT_BYTES, KEY_LIMIT_BYTES, Outcome, Transition, TxError, decide, truncated,
};
use crate::domain::activation::testing::RecordingNotifyMetrics;
use crate::domain::activation::{
    ActivationRecorder, ActivationRepository, ActivationState, ActivationStatus, BackResponse,
    NotifyMetrics, ReportResult, SubscriberTenant, Subscription, subscriber_of, tenant_text,
};
use crate::domain::error::DomainError;
use crate::infra::storage::activation_repo::ActivationRepo;
use crate::infra::storage::audit_store::AuditStore;
use crate::test_support::{sqlite_provider, sqlite_provider_acquiring_within};

const KEY: &str = "gts.cf.core.settings.setting_type.v1~acme.net.alpha.v1~";
const TENANT: Uuid = Uuid::from_u128(0xc1);
const CURRENT: Uuid = Uuid::from_u128(0xc0de);
const EARLIER: Uuid = Uuid::from_u128(0xea41);
const EXPECTED: &str = r#"{"a":2,"b":1}"#;

fn ctx(subscriber: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subscriber)
        .subject_tenant_id(TENANT)
        .subject_type("gts.cf.core.security.subject_service.v1~")
        .build()
        .expect("a context")
}

const GEAR: Uuid = Uuid::from_u128(0x6ea4);

struct Rig<R = ActivationRepo, A = AuditStore> {
    db: Arc<DBProvider<DbError>>,
    metrics: Arc<RecordingNotifyMetrics>,
    recorder: ActivationRecorder<R, A>,
}

impl Rig {
    async fn new() -> Self {
        Self::over(ActivationRepo, AuditStore).await
    }
}

impl<R, A> Rig<R, A>
where
    R: ActivationRepository + 'static,
    A: AuditSink + 'static,
{
    async fn over(repo: R, audit: A) -> Self {
        Self::on(sqlite_provider().await, repo, audit)
    }

    fn on(db: Arc<DBProvider<DbError>>, repo: R, audit: A) -> Self {
        let metrics = Arc::new(RecordingNotifyMetrics::default());
        let recorder = ActivationRecorder::new(
            Arc::clone(&db),
            repo,
            audit,
            Arc::clone(&metrics) as Arc<dyn NotifyMetrics>,
        );
        Self {
            db,
            metrics,
            recorder,
        }
    }

    /// An `awaiting` state of `GEAR` for `KEY` at `TENANT`, as a change wrote it.
    async fn seed(&self, expected: &str, change_ref: Uuid) {
        let conn = self.db.conn().expect("connection");
        let subscription: Subscription = ActivationRepo
            .register(&conn, &subscriber_of(&ctx(GEAR)), KEY, &tenant_text(TENANT))
            .await
            .expect("registered");
        ActivationRepo
            .write_states(
                &conn,
                &StateWrite {
                    key: KEY.to_owned(),
                    change_ref,
                    rows: vec![StateRow {
                        subscription_id: subscription.id,
                        tenant: tenant_text(TENANT),
                        expected_value: expected.to_owned(),
                    }],
                },
            )
            .await
            .expect("state written");
    }

    async fn state(&self) -> Option<ActivationState> {
        let conn = self.db.conn().expect("connection");
        ActivationRepo
            .state(&conn, &subscriber_of(&ctx(GEAR)), KEY, &tenant_text(TENANT))
            .await
            .expect("state read")
    }

    async fn records(&self) -> Vec<StoredAuditRecord> {
        let conn = self.db.conn().expect("connection");
        AuditStore
            .history(
                &conn,
                &AccessScope::allow_all(),
                KEY,
                TENANT,
                &toolkit_odata::ODataQuery::default(),
            )
            .await
            .expect("history")
            .items
            .into_iter()
            .filter(|r| r.operation == AuditOperation::Activation)
            .collect()
    }

    async fn report(&self, response: BackResponse) -> Result<ReportResult, DomainError> {
        self.recorder.receive_response(&ctx(GEAR), response).await
    }

    fn reports(&self) -> Vec<&'static str> {
        self.metrics.reports.lock().unwrap().clone()
    }
}

fn response(change_ref: Uuid, status: &str, applied: Option<&str>) -> BackResponse {
    BackResponse {
        change_ref: change_ref.to_string(),
        tenant: TENANT.to_string(),
        key: KEY.to_owned(),
        status: status.to_owned(),
        applied_value: applied.map(str::to_owned),
        detail: None,
    }
}

// ---------------------------------------------------------------------------
// The outcome rules, pure
// ---------------------------------------------------------------------------

fn row(change_ref: Uuid, expected: &str) -> ActivationState {
    ActivationState {
        subscriber: "gear".to_owned(),
        key: KEY.to_owned(),
        tenant: tenant_text(TENANT),
        change_ref,
        expected_value: expected.to_owned(),
        status: ActivationStatus::Awaiting,
        applied_value: None,
        detail: None,
        reported_at: None,
        last_notified_at: None,
        updated_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}

#[test]
fn the_outcome_table_of_the_rules() {
    let state = row(CURRENT, EXPECTED);
    let cases = [
        // success + match, whatever change_ref the report names
        (CURRENT, true, Some(EXPECTED), Outcome::Succeeded),
        (EARLIER, true, Some(EXPECTED), Outcome::Succeeded),
        // success + a different, missing or malformed value
        (CURRENT, true, Some(r#"{"a":3,"b":1}"#), Outcome::Failed),
        (CURRENT, true, None, Outcome::Failed),
        (CURRENT, true, Some("not json"), Outcome::Failed),
        // the same value in another serialization is not canonical, so it differs
        (CURRENT, true, Some(r#"{"b":1,"a":2}"#), Outcome::Failed),
        (EARLIER, true, Some(r#"{"a":3,"b":1}"#), Outcome::Ignored),
        (EARLIER, true, None, Outcome::Ignored),
        // failed names the current change or an earlier one
        (CURRENT, false, None, Outcome::Failed),
        (CURRENT, false, Some(EXPECTED), Outcome::Failed),
        (EARLIER, false, None, Outcome::Ignored),
    ];
    for (change_ref, success, applied, expected) in cases {
        assert_eq!(
            decide(&state, change_ref, success, applied),
            expected,
            "ref {change_ref}, success {success}, applied {applied:?}"
        );
    }
}

#[test]
fn a_detail_is_cut_at_one_kib_on_a_character_boundary() {
    let short = "short".to_owned();
    assert_eq!(truncated(short.clone()), short);
    let exact = "x".repeat(DETAIL_LIMIT_BYTES);
    assert_eq!(truncated(exact.clone()), exact);
    assert_eq!(truncated("x".repeat(5000)).len(), DETAIL_LIMIT_BYTES);
    // A two-byte character straddling the limit is dropped whole.
    let straddling = format!("{}é", "x".repeat(DETAIL_LIMIT_BYTES - 1));
    let cut = truncated(straddling);
    assert_eq!(cut.len(), DETAIL_LIMIT_BYTES - 1);
    assert!(cut.chars().all(|c| c == 'x'));
}

// ---------------------------------------------------------------------------
// A report on an awaiting state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_matching_value_settles_the_state_succeeded_and_appends_one_record() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let before = rig.state().await.expect("seeded");

    let outcome = rig
        .report(response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("answered");

    assert_eq!(outcome, ReportResult::Applied);
    let state = rig.state().await.expect("kept");
    assert_eq!(state.status, ActivationStatus::Succeeded);
    assert_eq!(state.applied_value.as_deref(), Some(EXPECTED));
    assert_eq!(state.detail, None);
    assert!(state.reported_at.is_some());
    assert_eq!(state.expected_value, before.expected_value, "never touched");
    assert_eq!(state.change_ref, before.change_ref, "never touched");
    assert_eq!(rig.records().await.len(), 1);
    assert_eq!(rig.reports(), ["applied"]);
    assert_eq!(*rig.metrics.reactions.lock().unwrap(), ["succeeded"]);
    assert_eq!(
        rig.metrics.latencies.lock().unwrap().len(),
        1,
        "the response latency is observed from the row's updated_at"
    );
}

#[tokio::test]
async fn a_matching_value_naming_an_earlier_change_still_settles_succeeded() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let outcome = rig
        .report(response(EARLIER, "success", Some(EXPECTED)))
        .await
        .expect("answered");
    assert_eq!(outcome, ReportResult::Applied);
    assert_eq!(
        rig.state().await.unwrap().status,
        ActivationStatus::Succeeded
    );
}

#[tokio::test]
async fn a_wrong_value_on_the_current_change_settles_failed() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let mut report = response(CURRENT, "success", Some(r#"{"a":3,"b":1}"#));
    report.detail = Some("ignored: a success carries no detail".to_owned());
    let outcome = rig.report(report).await.expect("answered");
    assert_eq!(outcome, ReportResult::Applied);
    let state = rig.state().await.unwrap();
    assert_eq!(state.status, ActivationStatus::Failed);
    assert_eq!(state.applied_value.as_deref(), Some(r#"{"a":3,"b":1}"#));
    assert_eq!(*rig.metrics.reactions.lock().unwrap(), ["failed"]);
}

#[tokio::test]
async fn a_success_without_a_value_or_with_a_malformed_one_settles_failed() {
    for applied in [None, Some("{not json"), Some(r#"{"b":1,"a":2}"#)] {
        let rig = Rig::new().await;
        rig.seed(EXPECTED, CURRENT).await;
        let outcome = rig
            .report(response(CURRENT, "success", applied))
            .await
            .expect("answered");
        assert_eq!(outcome, ReportResult::Applied, "{applied:?}");
        let state = rig.state().await.unwrap();
        assert_eq!(state.status, ActivationStatus::Failed, "{applied:?}");
        assert_eq!(state.applied_value.as_deref(), applied, "as reported");
    }
}

#[tokio::test]
async fn a_failed_report_on_the_current_change_settles_failed_with_its_detail() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let mut report = response(CURRENT, "failed", None);
    report.detail = Some("the pool could not be resized".to_owned());
    assert_eq!(
        rig.report(report).await.expect("answered"),
        ReportResult::Applied
    );
    let state = rig.state().await.unwrap();
    assert_eq!(state.status, ActivationStatus::Failed);
    assert_eq!(
        state.detail.as_deref(),
        Some("the pool could not be resized")
    );
    assert_eq!(
        state.applied_value, None,
        "absent when the report carried none"
    );
}

#[tokio::test]
async fn stale_reports_are_ignored_and_leave_the_state_awaiting() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    for report in [
        response(EARLIER, "success", Some(r#"{"a":3,"b":1}"#)),
        response(EARLIER, "success", None),
        response(EARLIER, "failed", None),
    ] {
        assert_eq!(
            rig.report(report).await.expect("answered"),
            ReportResult::Ignored
        );
    }
    assert_eq!(
        rig.state().await.unwrap().status,
        ActivationStatus::Awaiting
    );
    assert!(
        rig.records().await.is_empty(),
        "ignored reports write no record"
    );
    assert_eq!(rig.reports(), ["ignored"; 3]);
    assert!(rig.metrics.reactions.lock().unwrap().is_empty());
    assert!(rig.metrics.latencies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_repeated_report_makes_one_transition_and_one_record() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let first = rig
        .report(response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("answered");
    let repeat = rig
        .report(response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("answered");
    assert_eq!(
        (first, repeat),
        (ReportResult::Applied, ReportResult::Ignored)
    );
    assert_eq!(rig.records().await.len(), 1);
    assert_eq!(rig.reports(), ["applied", "ignored"]);
}

#[tokio::test]
async fn a_report_never_moves_a_terminal_state() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    rig.report(response(CURRENT, "failed", None))
        .await
        .expect("settles failed");
    let outcome = rig
        .report(response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("answered");
    assert_eq!(outcome, ReportResult::Ignored);
    assert_eq!(rig.state().await.unwrap().status, ActivationStatus::Failed);
    assert_eq!(rig.records().await.len(), 1);
}

#[tokio::test]
async fn a_report_for_no_state_is_ignored() {
    let rig = Rig::new().await;
    let outcome = rig
        .report(response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("answered");
    assert_eq!(outcome, ReportResult::Ignored);
    assert!(rig.records().await.is_empty());
    assert_eq!(rig.reports(), ["ignored"]);
}

#[tokio::test]
async fn another_subscribers_state_is_not_reported_on() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let outcome = rig
        .recorder
        .receive_response(
            &ctx(Uuid::from_u128(0x0e5e)),
            response(CURRENT, "success", Some(EXPECTED)),
        )
        .await
        .expect("answered");
    assert_eq!(
        outcome,
        ReportResult::Ignored,
        "the subscriber is the caller's"
    );
    assert_eq!(
        rig.state().await.unwrap().status,
        ActivationStatus::Awaiting
    );
}

#[tokio::test]
async fn a_secret_row_compares_the_hash_byte_for_byte() {
    let hash = "9f".repeat(32);
    for (applied, settles) in [
        (hash.clone(), ActivationStatus::Succeeded),
        ("00".repeat(32), ActivationStatus::Failed),
        (hash.to_uppercase(), ActivationStatus::Failed),
        (hash[..63].to_owned(), ActivationStatus::Failed),
    ] {
        let rig = Rig::new().await;
        rig.seed(&hash, CURRENT).await;
        rig.report(response(CURRENT, "success", Some(&applied)))
            .await
            .expect("answered");
        assert_eq!(rig.state().await.unwrap().status, settles, "{applied}");
    }
}

// ---------------------------------------------------------------------------
// The activation audit record
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_record_of_a_success_carries_the_stated_fields_and_no_images() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    rig.report(response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("answered");
    let records = rig.records().await;
    let [record] = records.as_slice() else {
        panic!("one record, got {}", records.len());
    };
    assert_eq!(record.declaration_key, KEY);
    assert_eq!(record.tenant_id, Some(TENANT));
    assert_eq!(record.actor, GEAR.to_string());
    assert_eq!(record.actor_classification, ActorClassification::Public);
    assert_eq!(record.outcome, AuditOutcome::Success);
    assert_eq!(record.change_ref, Some(CURRENT));
    assert_eq!(record.detail, None);
    assert_eq!(record.pre_image, None);
    assert_eq!(record.post_image, None);
    assert!(
        Uuid::parse_str(&record.request_id).is_ok(),
        "a minted request id"
    );
}

#[tokio::test]
async fn the_record_of_a_failure_carries_the_detail_truncated_to_one_kib() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let mut report = response(CURRENT, "failed", None);
    report.detail = Some("d".repeat(5000));
    rig.report(report).await.expect("answered");
    let records = rig.records().await;
    let [record] = records.as_slice() else {
        panic!("one record");
    };
    assert_eq!(record.outcome, AuditOutcome::Failure);
    assert_eq!(
        record.detail.as_deref().map(str::len),
        Some(DETAIL_LIMIT_BYTES)
    );
    assert_eq!(
        rig.state().await.unwrap().detail.as_deref().map(str::len),
        Some(DETAIL_LIMIT_BYTES),
        "the row keeps the same truncated detail"
    );
}

// ---------------------------------------------------------------------------
// Malformed input and failures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn malformed_input_is_an_invalid_argument_with_nothing_changed_or_counted() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let mut bad_tenant = response(CURRENT, "success", Some(EXPECTED));
    bad_tenant.tenant = "not-a-uuid".to_owned();
    let mut bad_ref = response(CURRENT, "success", Some(EXPECTED));
    bad_ref.change_ref = "also not".to_owned();
    let bad_status = response(CURRENT, "done", Some(EXPECTED));
    for report in [bad_tenant, bad_ref, bad_status] {
        let outcome = rig.report(report).await;
        assert!(
            matches!(outcome, Err(DomainError::Validation { .. })),
            "{outcome:?}"
        );
    }
    assert_eq!(
        rig.state().await.unwrap().status,
        ActivationStatus::Awaiting
    );
    assert!(rig.records().await.is_empty());
    assert!(rig.reports().is_empty(), "counted on no metric of the flow");
}

/// An audit sink whose append always fails.
struct FailingAudit;

#[async_trait]
impl AuditSink for FailingAudit {
    async fn append<C: DBRunner>(
        &self,
        _conn: &C,
        _scope: &AccessScope,
        _record: AuditRecord,
    ) -> Result<(), DomainError> {
        Err(DomainError::Unavailable {
            detail: "the audit table cannot be written".to_owned(),
        })
    }
}

#[tokio::test]
async fn a_failed_audit_append_rolls_the_transition_back_and_answers_an_error() {
    let rig = Rig::over(ActivationRepo, FailingAudit).await;
    rig.seed(EXPECTED, CURRENT).await;
    let outcome = rig
        .report(response(CURRENT, "success", Some(EXPECTED)))
        .await;
    assert!(matches!(outcome, Err(DomainError::Unavailable { .. })));
    let state = rig.state().await.unwrap();
    assert_eq!(state.status, ActivationStatus::Awaiting, "nothing changed");
    assert_eq!(state.applied_value, None);
    assert_eq!(state.reported_at, None);
    assert!(rig.reports().is_empty(), "counted on no metric");
    assert!(rig.metrics.reactions.lock().unwrap().is_empty());
}

#[test]
fn a_failed_commit_answers_unavailable_and_changes_nothing() {
    // `transaction_ref_mapped` maps a failed begin or commit through
    // `From<DbError>`; the Recorder's boundary turns that arm into an outage,
    // and leaves a statement's own error alone. A commit that fails changes
    // nothing by the transaction's own atomicity.
    let commit = DomainError::from(TxError::from(DbError::InvalidParameter(
        "commit refused".to_owned(),
    )));
    assert!(
        matches!(commit, DomainError::Unavailable { .. }),
        "{commit:?}"
    );
    let statement = DomainError::from(TxError::Statement(DomainError::Internal {
        diagnostic: "a statement".to_owned(),
    }));
    assert!(
        matches!(statement, DomainError::Internal { .. }),
        "{statement:?}"
    );
}

#[tokio::test]
async fn a_transaction_that_cannot_begin_answers_unavailable_and_changes_nothing() {
    let db = sqlite_provider_acquiring_within(std::time::Duration::from_millis(100)).await;
    let rig = Rig::on(Arc::clone(&db), ActivationRepo, AuditStore);
    rig.seed(EXPECTED, CURRENT).await;
    // Another transaction holds the pool's one connection.
    let held = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let holder = {
        let (db, held, release) = (Arc::clone(&db), Arc::clone(&held), Arc::clone(&release));
        tokio::spawn(async move {
            db.db()
                .transaction_ref_mapped::<_, (), TxError>(move |_tx| {
                    Box::pin(async move {
                        held.notify_one();
                        release.notified().await;
                        Ok(())
                    })
                })
                .await
                .ok();
        })
    };
    held.notified().await;
    let outcome = rig
        .report(response(CURRENT, "success", Some(EXPECTED)))
        .await;
    release.notify_one();
    holder.await.unwrap();
    assert!(
        matches!(outcome, Err(DomainError::Unavailable { .. })),
        "{outcome:?}"
    );
    let state = rig.state().await.unwrap();
    assert_eq!(state.status, ActivationStatus::Awaiting, "nothing changed");
    assert!(rig.records().await.is_empty());
    assert!(rig.reports().is_empty(), "counted on no metric");
}

#[tokio::test]
async fn a_nul_byte_or_an_overlong_key_is_an_invalid_argument_with_nothing_changed() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let mut in_key = response(CURRENT, "success", Some(EXPECTED));
    in_key.key = format!("{KEY}\0");
    let mut in_detail = response(CURRENT, "failed", None);
    in_detail.detail = Some("a\0b".to_owned());
    let in_value = response(CURRENT, "success", Some("{\"a\":\"\\u0000\"}\0"));
    let mut long_key = response(CURRENT, "success", Some(EXPECTED));
    long_key.key = "k".repeat(KEY_LIMIT_BYTES + 1);
    for (name, report) in [
        ("key", in_key),
        ("detail", in_detail),
        ("applied_value", in_value),
        ("key", long_key),
    ] {
        match rig.report(report).await {
            Err(DomainError::Validation { field, .. }) => assert_eq!(field, name),
            other => panic!("{name}: {other:?}"),
        }
    }
    assert_eq!(
        rig.state().await.unwrap().status,
        ActivationStatus::Awaiting
    );
    assert!(rig.records().await.is_empty());
    assert!(rig.reports().is_empty());
}

#[tokio::test]
async fn an_applied_value_above_the_size_cap_does_not_match_and_is_not_stored() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let mut report = response(
        CURRENT,
        "success",
        Some(&"x".repeat(crate::domain::validation::guards::MAX_SERIALIZED_BYTES + 1)),
    );
    report.detail = Some("ignored on success".to_owned());
    assert_eq!(rig.report(report).await.unwrap(), ReportResult::Applied);
    let state = rig.state().await.unwrap();
    assert_eq!(state.status, ActivationStatus::Failed);
    assert_eq!(
        state.applied_value, None,
        "the oversized value is not stored"
    );
}

#[tokio::test]
async fn an_oversized_value_on_a_failed_report_keeps_the_detail() {
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let mut report = response(
        CURRENT,
        "failed",
        Some(&"x".repeat(crate::domain::validation::guards::MAX_SERIALIZED_BYTES + 1)),
    );
    report.detail = Some("could not apply".to_owned());
    rig.report(report).await.unwrap();
    let state = rig.state().await.unwrap();
    assert_eq!(state.applied_value, None);
    assert_eq!(state.detail.as_deref(), Some("could not apply"));
}

#[test]
fn debug_output_redacts_the_value_and_the_detail() {
    let mut report = response(CURRENT, "failed", Some("SECRET-VALUE-9"));
    report.detail = Some("SECRET-DETAIL-9".to_owned());
    let transition = Transition {
        subscriber: "gear".to_owned(),
        key: KEY.to_owned(),
        tenant: tenant_text(TENANT),
        change_ref: CURRENT,
        status: ActivationStatus::Failed,
        applied_value: Some("SECRET-VALUE-9".to_owned()),
        detail: Some("SECRET-DETAIL-9".to_owned()),
    };
    let parsed = super::parse(report.clone()).expect("parses");
    for text in [
        format!("{report:?}"),
        format!("{transition:?}"),
        format!("{parsed:?}"),
    ] {
        assert!(
            !text.contains("SECRET-VALUE-9") && !text.contains("SECRET-DETAIL-9"),
            "{text}"
        );
        assert!(text.contains(KEY), "the key stays: {text}");
    }
}

/// A repository that lets a newer change replace the row between the Recorder's
/// read and its guarded update, as a concurrent write would.
struct RacingRepo {
    replacement: Uuid,
}

#[async_trait]
impl ActivationRepository for RacingRepo {
    async fn register<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Subscription, DomainError> {
        ActivationRepo.register(conn, subscriber, key, tenant).await
    }

    async fn of_key<C: DBRunner>(
        &self,
        conn: &C,
        key: &str,
    ) -> Result<Vec<Subscription>, DomainError> {
        ActivationRepo.of_key(conn, key).await
    }

    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Option<Subscription>, DomainError> {
        ActivationRepo.find(conn, subscriber, key, tenant).await
    }

    async fn retire<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<bool, DomainError> {
        ActivationRepo.retire(conn, subscriber, key, tenant).await
    }

    async fn write_states<C: DBRunner>(
        &self,
        conn: &C,
        write: &StateWrite,
    ) -> Result<Vec<SubscriberTenant>, DomainError> {
        ActivationRepo.write_states(conn, write).await
    }

    async fn state<C: DBRunner>(
        &self,
        conn: &C,
        subscriber: &str,
        key: &str,
        tenant: &str,
    ) -> Result<Option<ActivationState>, DomainError> {
        ActivationRepo.state(conn, subscriber, key, tenant).await
    }

    async fn report<C: DBRunner>(
        &self,
        conn: &C,
        transition: &Transition,
    ) -> Result<Option<time::OffsetDateTime>, DomainError> {
        // The newer change lands after the decision was taken.
        let subscription = ActivationRepo
            .find(
                conn,
                &transition.subscriber,
                &transition.key,
                &transition.tenant,
            )
            .await?
            .expect("subscribed");
        ActivationRepo
            .write_states(
                conn,
                &StateWrite {
                    key: transition.key.clone(),
                    change_ref: self.replacement,
                    rows: vec![StateRow {
                        subscription_id: subscription.id,
                        tenant: transition.tenant.clone(),
                        expected_value: r#"{"a":9}"#.to_owned(),
                    }],
                },
            )
            .await?;
        ActivationRepo.report(conn, transition).await
    }
}

#[tokio::test]
async fn a_report_on_a_replaced_expectation_is_ignored_without_a_record() {
    let newer = Uuid::from_u128(0x4e3e4);
    let rig = Rig::over(ActivationRepo, AuditStore).await;
    rig.seed(EXPECTED, CURRENT).await;
    // The same database, now under the racing repository.
    let racing = ActivationRecorder::new(
        Arc::clone(&rig.db),
        RacingRepo { replacement: newer },
        AuditStore,
        Arc::clone(&rig.metrics) as Arc<dyn NotifyMetrics>,
    );
    let outcome = racing
        .receive_response(&ctx(GEAR), response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("answered");
    assert_eq!(outcome, ReportResult::Ignored);
    let state = rig.state().await.unwrap();
    assert_eq!(
        state.change_ref, newer,
        "the state ends naming the newer change"
    );
    assert_eq!(state.status, ActivationStatus::Awaiting);
    assert!(
        rig.records().await.is_empty(),
        "no audit record for the ignored report"
    );
    assert_eq!(rig.reports(), ["ignored"]);
}

#[tokio::test]
async fn no_value_hash_or_secret_reaches_a_log_line_or_a_metric_label() {
    use crate::domain::activation::testing::{capture, logged};
    let hash = "9f".repeat(32);
    let rig = Rig::new().await;
    rig.seed(&hash, CURRENT).await;
    let (buf, _guard) = capture();
    let mut wrong = response(CURRENT, "success", Some(&"ab".repeat(32)));
    wrong.detail = Some("the pool could not be resized".to_owned());
    rig.report(wrong).await.expect("answered");
    rig.report(response(EARLIER, "success", Some(&hash)))
        .await
        .expect("answered");
    let log = logged(&buf);
    for secret in [hash.as_str(), &"ab".repeat(32)] {
        assert!(!log.contains(secret), "a hash in the log: {log}");
    }
    for label in rig
        .metrics
        .reports
        .lock()
        .unwrap()
        .iter()
        .chain(rig.metrics.reactions.lock().unwrap().iter())
    {
        assert!(
            ["applied", "ignored", "succeeded", "failed"].contains(label),
            "a metric label outside the closed vocabulary: {label}"
        );
    }
}
