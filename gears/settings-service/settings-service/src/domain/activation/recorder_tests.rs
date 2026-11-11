//! The Activation Outcome Recorder over `SQLite`: the outcome rules, the
//! guarded transition, and the `activation` audit record written with it.
//!
//! Level 1 rows of DESIGN-activation §7 (group B): *Matching value succeeds*
//! (on a `silent` state too), *Wrong value with the current `change_ref`*,
//! *Late failure*, *Stale report ignored*, *Stale report re-arms a silent
//! row*, *Repeat report*, *No row*, *No subscription*, *Call without
//! `instance_id`*, *Report touches only its own instance*, *Activation audit
//! record once per transition*, *Report result counted*, *Outcome status —
//! secret hash compare*, the response latency from `changed_at` with its
//! `late` label, and the error rows of the report flow. The concurrent rows
//! run on `PostgreSQL` (`tests/pg_activation_delivery_test.rs`).
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
use crate::domain::activation::InstanceKey;
use crate::domain::activation::publisher::{StateRow, StateWrite};
use crate::domain::activation::recorder::{
    DETAIL_LIMIT_BYTES, KEY_LIMIT_BYTES, Outcome, Transition, TxError, decide, truncated,
};
use crate::domain::activation::testing::RecordingNotifyMetrics;
use crate::domain::activation::testing::{ONE_INSTANCE, one};
use crate::domain::activation::{
    ActivationRecorder, ActivationRepository, ActivationState, ActivationStatus, BackResponse,
    NotifyMetrics, ReportResult, Subscription, subscriber_of, tenant_text,
};
use crate::domain::error::DomainError;
use crate::infra::storage::activation_repo::ActivationRepo;
use crate::infra::storage::audit_store::AuditStore;
use crate::test_support::activation_repo_forward::forwarding_activation_repo;
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

    /// An `awaiting` state of `GEAR`'s one instance for `KEY` at `TENANT`, as a
    /// change wrote it.
    async fn seed(&self, expected: &str, change_ref: Uuid) {
        let subscription = self.register(ONE_INSTANCE, None).await;
        self.change(expected, change_ref, &[&subscription]).await;
    }

    /// Register instance `instance_id` of `GEAR` for `KEY` at `TENANT`.
    async fn register(&self, instance_id: Uuid, label: Option<&str>) -> Subscription {
        let conn = self.db.conn().expect("connection");
        ActivationRepo
            .register(&conn, &instance(instance_id), label)
            .await
            .expect("registered")
    }

    /// A change of `KEY` whose read set is `subscriptions`.
    async fn change(&self, expected: &str, change_ref: Uuid, subscriptions: &[&Subscription]) {
        let conn = self.db.conn().expect("connection");
        ActivationRepo
            .write_states(
                &conn,
                &StateWrite {
                    key: KEY.to_owned(),
                    change_ref,
                    rows: subscriptions
                        .iter()
                        .map(|s| StateRow {
                            subscription_id: s.id,
                            tenant: tenant_text(TENANT),
                            expected_value: expected.to_owned(),
                        })
                        .collect(),
                },
            )
            .await
            .expect("states written");
    }

    /// Count one accepted delivery on every due state, then call the
    /// instance's state of `change_ref` silent and mark its subscription, as
    /// the delivery loop does.
    async fn silence(&self, instance_id: Uuid, change_ref: Uuid) {
        let conn = self.db.conn().expect("connection");
        let at = time::OffsetDateTime::now_utc();
        let due = ActivationRepo
            .due(&conn, at + time::Duration::days(1))
            .await
            .expect("due");
        ActivationRepo
            .stamp_due(&conn, &due, at)
            .await
            .expect("stamped");
        assert!(
            ActivationRepo
                .call_silent(&conn, &instance(instance_id), change_ref, at)
                .await
                .expect("called silent"),
            "the state was awaiting the change"
        );
        ActivationRepo
            .mark_silent(&conn, &instance(instance_id), at)
            .await
            .expect("marked");
    }

    async fn state_of(&self, instance_id: Uuid) -> Option<ActivationState> {
        let conn = self.db.conn().expect("connection");
        ActivationRepo
            .state(&conn, &instance(instance_id))
            .await
            .expect("state read")
    }

    async fn subscription_of(&self, instance_id: Uuid) -> Option<Subscription> {
        let conn = self.db.conn().expect("connection");
        ActivationRepo
            .find(&conn, &instance(instance_id))
            .await
            .expect("subscription read")
    }

    /// Move the state's `changed_at` back by `by`, as if its change committed
    /// that long ago; nothing else of the row moves.
    async fn backdate(&self, instance_id: Uuid, by: time::Duration) {
        use crate::infra::storage::entity::activation_state::{Column, Entity};
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use toolkit_db::secure::SecureUpdateExt;
        let changed_at = self
            .state_of(instance_id)
            .await
            .expect("a state")
            .changed_at;
        let conn = self.db.conn().expect("connection");
        Entity::update_many()
            .col_expr(Column::ChangedAt, Expr::value(changed_at - by))
            .filter(Column::InstanceId.eq(instance_id))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await
            .expect("backdated");
    }

    async fn state(&self) -> Option<ActivationState> {
        let conn = self.db.conn().expect("connection");
        ActivationRepo
            .state(
                &conn,
                &InstanceKey {
                    subscriber: subscriber_of(&ctx(GEAR)),
                    instance_id: Uuid::nil(),
                    key: KEY.to_owned(),
                    tenant: tenant_text(TENANT),
                },
            )
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

    /// A report of `GEAR`'s instance `instance_id`.
    async fn report_as(
        &self,
        instance_id: Uuid,
        mut response: BackResponse,
    ) -> Result<ReportResult, DomainError> {
        response.instance_id = Some(instance_id);
        self.report(response).await
    }

    fn reports(&self) -> Vec<&'static str> {
        self.metrics.reports.lock().unwrap().clone()
    }
}

/// The identity of `GEAR`'s instance `instance_id` for `KEY` at `TENANT`.
fn instance(instance_id: Uuid) -> InstanceKey {
    InstanceKey {
        instance_id,
        ..one(&subscriber_of(&ctx(GEAR)), KEY, &tenant_text(TENANT))
    }
}

/// Instance `n` of a subscriber running several processes.
fn nth(n: u128) -> Uuid {
    Uuid::from_u128(0x1000 + n)
}

fn response(change_ref: Uuid, status: &str, applied: Option<&str>) -> BackResponse {
    BackResponse {
        instance_id: Some(ONE_INSTANCE),
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
        instance_id: Uuid::nil(),
        key: KEY.to_owned(),
        tenant: tenant_text(TENANT),
        change_ref,
        changed_at: time::OffsetDateTime::UNIX_EPOCH,
        expected_value: expected.to_owned(),
        status: ActivationStatus::Awaiting,
        deliveries: 0,
        silent_since: None,
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
    let latencies = rig.metrics.latencies.lock().unwrap().clone();
    assert_eq!(latencies.len(), 1, "one response latency observed");
    assert!(!latencies[0].1, "settling an awaiting state is not late");
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
    assert_eq!(rig.reports(), ["ignored_stale"; 3]);
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
    assert_eq!(rig.reports(), ["applied", "ignored_repeat"]);
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
async fn a_report_of_an_instance_without_a_subscription_is_not_subscribed_and_writes_nothing() {
    // Level 1 row *No subscription*: a sibling's state is there, the
    // reporting instance holds no subscription of its own.
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let outcome = rig
        .report_as(nth(9), response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("answered");
    assert_eq!(outcome, ReportResult::NotSubscribed);
    assert!(rig.state_of(nth(9)).await.is_none());
    assert!(rig.subscription_of(nth(9)).await.is_none());
    assert_eq!(
        rig.state().await.unwrap().status,
        ActivationStatus::Awaiting,
        "the sibling's state is not this instance's"
    );
    assert!(rig.records().await.is_empty());
    assert_eq!(rig.reports(), ["not_subscribed"]);
    assert!(rig.metrics.reactions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_report_of_a_subscribed_instance_without_a_state_is_ignored_no_row() {
    // Level 1 row *No row*: a subscribed instance with no state of the
    // current change.
    let rig = Rig::new().await;
    rig.register(ONE_INSTANCE, None).await;
    let outcome = rig
        .report(response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("answered");
    assert_eq!(outcome, ReportResult::Ignored);
    assert!(rig.state().await.is_none(), "no state is created");
    assert!(rig.records().await.is_empty());
    assert_eq!(rig.reports(), ["ignored_no_row"]);
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
        ReportResult::NotSubscribed,
        "the subscriber is the caller's: the other gear holds no subscription"
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
        instance_id: Uuid::nil(),
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

/// A repository that lets a concurrent writer land on the row between the
/// Recorder's read and its guarded update.
struct RacingRepo {
    race: Race,
}

/// What lands between the read and the guarded update.
enum Race {
    /// A newer change replaces the row, naming this `change_ref`.
    Replace(Uuid),
    /// The delivery loop calls the row silent.
    CallSilent,
}

forwarding_activation_repo! {
    impl ActivationRepository for RacingRepo {
        async fn report<C: DBRunner>(
            &self,
            conn: &C,
            transition: &Transition,
        ) -> Result<Option<time::OffsetDateTime>, DomainError> {
            let replacement = match self.race {
                Race::Replace(replacement) => replacement,
                Race::CallSilent => {
                    // The call of silence commits after the decision was taken.
                    let instance = InstanceKey {
                        subscriber: transition.subscriber.clone(),
                        instance_id: transition.instance_id,
                        key: transition.key.clone(),
                        tenant: transition.tenant.clone(),
                    };
                    let at = crate::infra::storage::clock::now();
                    assert!(
                        ActivationRepo
                            .call_silent(conn, &instance, transition.change_ref, at)
                            .await?,
                        "the call of silence matched the awaiting row"
                    );
                    ActivationRepo.mark_silent(conn, &instance, at).await?;
                    return ActivationRepo.report(conn, transition).await;
                }
            };
            // The newer change lands after the decision was taken.
            let subscription = ActivationRepo
                .find(
                    conn,
                    &InstanceKey {
                        subscriber: transition.subscriber.clone(),
                        instance_id: transition.instance_id,
                        key: transition.key.clone(),
                        tenant: transition.tenant.clone(),
                    },
                )
                .await?
                .expect("subscribed");
            ActivationRepo
                .write_states(
                    conn,
                    &StateWrite {
                        key: transition.key.clone(),
                        change_ref: replacement,
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
    forwarded: [
        register, of_key, find, registrations,
        oldest_evictable, silent_subscriptions, unsubscribe, delete_key,
        write_states, state, rearm, clear_silent,
        states_of, activation_rows,
    ]
}

#[tokio::test]
async fn a_report_on_a_replaced_expectation_is_ignored_without_a_record() {
    let newer = Uuid::from_u128(0x4e3e4);
    let rig = Rig::over(ActivationRepo, AuditStore).await;
    rig.seed(EXPECTED, CURRENT).await;
    // The same database, now under the racing repository.
    let racing = ActivationRecorder::new(
        Arc::clone(&rig.db),
        RacingRepo {
            race: Race::Replace(newer),
        },
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
    assert_eq!(
        rig.reports(),
        ["ignored_stale"],
        "a guarded update that matched nothing"
    );
}

/// *Report against the call of silence*, the call landing between the
/// report's read and its guarded update: the read saw the state `awaiting`, the
/// update settles it `silent` — a late report, labelled `late="true"` from the
/// row the update settled, not from the read.
#[tokio::test]
async fn a_report_settling_a_state_called_silent_after_its_read_is_late() {
    let rig = Rig::over(ActivationRepo, AuditStore).await;
    rig.seed(EXPECTED, CURRENT).await;
    assert_eq!(
        rig.state().await.unwrap().status,
        ActivationStatus::Awaiting,
        "the report reads the state awaiting"
    );
    let racing = ActivationRecorder::new(
        Arc::clone(&rig.db),
        RacingRepo {
            race: Race::CallSilent,
        },
        AuditStore,
        Arc::clone(&rig.metrics) as Arc<dyn NotifyMetrics>,
    );
    let outcome = racing
        .receive_response(&ctx(GEAR), response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("answered");
    assert_eq!(outcome, ReportResult::Applied);
    let state = rig.state().await.unwrap();
    assert_eq!(state.status, ActivationStatus::Succeeded);
    assert!(
        state.silent_since.is_some(),
        "the settled row was silent and keeps its silent_since"
    );
    let latencies = rig.metrics.latencies.lock().unwrap().clone();
    assert_eq!(latencies.len(), 1, "one response latency observed");
    assert!(
        latencies[0].1,
        "settling the row the call of silence made silent is late"
    );
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
            [
                "applied",
                "not_subscribed",
                "ignored_no_row",
                "ignored_stale",
                "ignored_repeat",
                "succeeded",
                "failed"
            ]
            .contains(label),
            "a metric label outside the closed vocabulary: {label}"
        );
    }
}

// ---------------------------------------------------------------------------
// Per instance: the instance's own state, silence and re-entry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_call_without_an_instance_id_is_the_typed_invalid_input_with_nothing_changed() {
    // Level 1 row *Call without `instance_id`*: refused before every other
    // input rule, counted on no metric.
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let mut without = response(CURRENT, "success", Some(EXPECTED));
    without.instance_id = None;
    let mut also_malformed = without.clone();
    also_malformed.tenant = "not-a-uuid".to_owned();
    for report in [without, also_malformed] {
        match rig.report(report).await {
            Err(DomainError::Validation { field, code, .. }) => {
                assert_eq!(field, "instance_id");
                assert_eq!(code, crate::field::INSTANCE_ID_REQUIRED);
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(
        rig.state().await.unwrap().status,
        ActivationStatus::Awaiting
    );
    assert!(rig.records().await.is_empty());
    assert!(rig.reports().is_empty(), "counted on no metric of the flow");
}

/// `GEAR`'s subject under `subject_type`, or with none.
fn labelled(subject_type: Option<&str>) -> SecurityContext {
    let mut builder = SecurityContext::builder()
        .subject_id(GEAR)
        .subject_tenant_id(TENANT);
    if let Some(label) = subject_type {
        builder = builder.subject_type(label);
    }
    builder.build().expect("a context")
}

#[tokio::test]
async fn a_report_by_a_person_is_refused_with_nothing_read_changed_or_counted() {
    // The person's subject id is the subscriber of a live state: were the
    // report taken, it would settle that state and write a public record.
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    for label in [crate::domain::stepup::USER_SUBJECT_TYPE, "user"] {
        let outcome = rig
            .recorder
            .receive_response(
                &labelled(Some(label)),
                response(CURRENT, "success", Some(EXPECTED)),
            )
            .await;
        assert!(
            matches!(outcome, Err(DomainError::Unauthorized { .. })),
            "{outcome:?}"
        );
        // Refused before the input is parsed: a malformed report answers the same.
        let mut malformed = response(CURRENT, "success", Some(EXPECTED));
        malformed.instance_id = None;
        let outcome = rig
            .recorder
            .receive_response(&labelled(Some(label)), malformed)
            .await;
        assert!(
            matches!(outcome, Err(DomainError::Unauthorized { .. })),
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

#[tokio::test]
async fn a_report_whose_subject_carries_no_type_is_taken() {
    // A gear may build its context without a subject type: absence is no
    // evidence of a person.
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let answer = rig
        .recorder
        .receive_response(
            &labelled(None),
            response(CURRENT, "success", Some(EXPECTED)),
        )
        .await
        .expect("taken");
    assert_eq!(answer, ReportResult::Applied);
    assert_eq!(
        rig.state().await.unwrap().status,
        ActivationStatus::Succeeded
    );
    assert_eq!(rig.records().await.len(), 1);
}

#[tokio::test]
async fn a_report_settles_only_the_reporting_instances_state() {
    // Level 1 row *Report touches only its own instance*.
    let rig = Rig::new().await;
    let one = rig.register(nth(1), Some("host-1")).await;
    let two = rig.register(nth(2), Some("host-2")).await;
    rig.change(EXPECTED, CURRENT, &[&one, &two]).await;

    let outcome = rig
        .report_as(nth(1), response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("answered");

    assert_eq!(outcome, ReportResult::Applied);
    assert_eq!(
        rig.state_of(nth(1)).await.unwrap().status,
        ActivationStatus::Succeeded
    );
    let sibling = rig.state_of(nth(2)).await.unwrap();
    assert_eq!(sibling.status, ActivationStatus::Awaiting, "untouched");
    assert_eq!(sibling.reported_at, None);

    // Another gear naming this instance finds no subscription of its own and
    // touches nothing of `GEAR`.
    let foreign = rig
        .recorder
        .receive_response(&ctx(Uuid::from_u128(0x0e5e)), {
            let mut r = response(CURRENT, "failed", None);
            r.instance_id = Some(nth(2));
            r
        })
        .await
        .expect("answered");
    assert_eq!(foreign, ReportResult::NotSubscribed);
    assert_eq!(
        rig.state_of(nth(2)).await.unwrap().status,
        ActivationStatus::Awaiting
    );
    assert_eq!(rig.records().await.len(), 1);
}

#[tokio::test]
async fn a_matching_late_report_settles_a_silent_state_keeping_its_silent_since() {
    // Level 1 row *Matching value succeeds*, on a `silent` state.
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    rig.silence(ONE_INSTANCE, CURRENT).await;
    let silent = rig.state().await.unwrap();
    assert_eq!(silent.status, ActivationStatus::Silent);

    let outcome = rig
        .report(response(EARLIER, "success", Some(EXPECTED)))
        .await
        .expect("answered");

    assert_eq!(
        outcome,
        ReportResult::Applied,
        "whatever change_ref it names"
    );
    let state = rig.state().await.unwrap();
    assert_eq!(state.status, ActivationStatus::Succeeded);
    assert_eq!(
        state.silent_since, silent.silent_since,
        "the late confirmation stays visible"
    );
    assert_eq!(state.change_ref, CURRENT);
    assert_eq!(
        rig.subscription_of(ONE_INSTANCE)
            .await
            .unwrap()
            .silent_since,
        None,
        "the report ends the instance's silence"
    );
    assert_eq!(rig.records().await.len(), 1);
    assert_eq!(rig.reports(), ["applied"]);
    let latencies = rig.metrics.latencies.lock().unwrap().clone();
    assert_eq!(latencies.len(), 1);
    assert!(latencies[0].1, "settling a silent state is late");
}

#[tokio::test]
async fn a_late_failure_settles_a_silent_state_failed() {
    // Level 1 row *Late failure*.
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    rig.silence(ONE_INSTANCE, CURRENT).await;
    let silent_since = rig.state().await.unwrap().silent_since;

    let mut report = response(CURRENT, "failed", None);
    report.detail = Some("could not reload".to_owned());
    assert_eq!(
        rig.report(report).await.expect("answered"),
        ReportResult::Applied
    );

    let state = rig.state().await.unwrap();
    assert_eq!(state.status, ActivationStatus::Failed);
    assert_eq!(state.detail.as_deref(), Some("could not reload"));
    assert_eq!(state.silent_since, silent_since, "kept");
    let records = rig.records().await;
    let [record] = records.as_slice() else {
        panic!("one record, got {}", records.len());
    };
    assert_eq!(record.outcome, AuditOutcome::Failure);
    assert_eq!(record.detail.as_deref(), Some("could not reload"));
    assert_eq!(*rig.metrics.reactions.lock().unwrap(), ["failed"]);
    assert!(rig.metrics.latencies.lock().unwrap()[0].1, "late");
}

#[tokio::test]
async fn a_stale_report_on_an_awaiting_state_is_ignored_and_ends_the_instances_silence() {
    // Level 1 row *Stale report ignored*, clearing `silent_since`: the
    // instance's only state was called silent, then a newer change expected
    // it again — every instance of the subscriber being silent — leaving its
    // subscription marked.
    let rig = Rig::new().await;
    rig.seed(EXPECTED, EARLIER).await;
    rig.silence(ONE_INSTANCE, EARLIER).await;
    let subscription = rig.subscription_of(ONE_INSTANCE).await.unwrap();
    rig.change(EXPECTED, CURRENT, &[&subscription]).await;
    assert!(
        rig.subscription_of(ONE_INSTANCE)
            .await
            .unwrap()
            .silent_since
            .is_some()
    );

    let outcome = rig
        .report(response(EARLIER, "failed", None))
        .await
        .expect("answered");

    assert_eq!(outcome, ReportResult::Ignored);
    let state = rig.state().await.unwrap();
    assert_eq!(
        (state.change_ref, state.status),
        (CURRENT, ActivationStatus::Awaiting)
    );
    assert_eq!(
        rig.subscription_of(ONE_INSTANCE)
            .await
            .unwrap()
            .silent_since,
        None,
        "expected again from the next change"
    );
    assert!(rig.records().await.is_empty());
    assert_eq!(rig.reports(), ["ignored_stale"]);
}

#[tokio::test]
async fn a_stale_report_re_arms_the_instances_silent_state_of_the_current_change() {
    // Level 1 row *Stale report re-arms a silent row*.
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    rig.silence(ONE_INSTANCE, CURRENT).await;
    let silent = rig.state().await.unwrap();
    assert_eq!(silent.deliveries, 1);
    assert!(silent.last_notified_at.is_some());

    let outcome = rig
        .report(response(EARLIER, "success", Some(r#"{"a":3,"b":1}"#)))
        .await
        .expect("answered");

    assert_eq!(outcome, ReportResult::Ignored);
    let state = rig.state().await.unwrap();
    assert_eq!(
        state.status,
        ActivationStatus::Awaiting,
        "due at the next scan"
    );
    assert_eq!(state.deliveries, 0);
    assert_eq!((state.silent_since, state.last_notified_at), (None, None));
    assert_eq!(state.change_ref, CURRENT);
    assert_eq!(state.changed_at, silent.changed_at, "the baseline stays");
    assert_eq!((state.applied_value, state.reported_at), (None, None));
    assert_eq!(
        rig.subscription_of(ONE_INSTANCE)
            .await
            .unwrap()
            .silent_since,
        None,
        "its own re-arm leaves no silent state to keep the silence"
    );
    assert!(rig.records().await.is_empty(), "a re-arm writes no record");
    assert_eq!(rig.reports(), ["ignored_stale"]);
}

#[tokio::test]
async fn an_instance_left_out_as_silent_ends_its_silence_by_a_report_and_gets_no_state() {
    // Level 1 row *No row*: silent while a sibling was expected, the instance
    // stays out of the change; its report still ends its silence.
    let rig = Rig::new().await;
    let one = rig.register(nth(1), None).await;
    let two = rig.register(nth(2), None).await;
    rig.change(EXPECTED, EARLIER, &[&one, &two]).await;
    rig.silence(nth(2), EARLIER).await;
    rig.change(EXPECTED, CURRENT, &[&one, &two]).await;
    assert!(rig.state_of(nth(2)).await.is_none(), "left out");

    let outcome = rig
        .report_as(nth(2), response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("answered");

    assert_eq!(outcome, ReportResult::Ignored);
    assert!(rig.state_of(nth(2)).await.is_none(), "no state is created");
    assert_eq!(
        rig.subscription_of(nth(2)).await.unwrap().silent_since,
        None,
        "expected again from the next change"
    );
    assert_eq!(
        rig.state_of(nth(1)).await.unwrap().status,
        ActivationStatus::Awaiting
    );
    assert!(rig.records().await.is_empty());
    assert_eq!(rig.reports(), ["ignored_no_row"]);
}

#[tokio::test]
async fn a_repeat_on_a_settled_state_re_arms_nothing_and_is_counted_a_repeat() {
    // Level 1 row *Repeat report*: the same end state as the first report.
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    rig.silence(ONE_INSTANCE, CURRENT).await;
    rig.report(response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("settles late");
    let settled = rig.state().await.unwrap();

    for repeat in [
        response(CURRENT, "success", Some(EXPECTED)),
        response(EARLIER, "failed", None),
    ] {
        assert_eq!(
            rig.report(repeat).await.expect("answered"),
            ReportResult::Ignored
        );
    }

    assert_eq!(rig.state().await.unwrap(), settled, "nothing moved");
    assert_eq!(rig.records().await.len(), 1);
    assert_eq!(
        rig.reports(),
        ["applied", "ignored_repeat", "ignored_repeat"]
    );
}

#[tokio::test]
async fn the_record_names_the_reporting_instance_and_its_label_once_per_transition() {
    // Level 1 row *Activation audit record once per transition*, from
    // `awaiting` and from `silent`.
    let rig = Rig::new().await;
    let one = rig.register(nth(1), Some("host-1")).await;
    let two = rig.register(nth(2), Some("host-2")).await;
    rig.change(EXPECTED, CURRENT, &[&one, &two]).await;
    rig.silence(nth(2), CURRENT).await;
    // The label as the subscription holds it at the time of the report: the
    // repository's refresh of a repeat registration, which touches nothing
    // else.
    rig.register(nth(2), Some("host-2b")).await;

    for n in [1, 2] {
        for _ in 0..2 {
            rig.report_as(nth(n), response(CURRENT, "success", Some(EXPECTED)))
                .await
                .expect("answered");
        }
    }

    let mut records = rig.records().await;
    records.sort_by_key(|r| r.instance_id);
    let labels: Vec<_> = records
        .iter()
        .map(|r| (r.instance_id, r.instance_label.as_deref()))
        .collect();
    assert_eq!(
        labels,
        [
            (Some(nth(1)), Some("host-1")),
            (Some(nth(2)), Some("host-2b"))
        ],
        "one record per transition, each naming its instance"
    );
    for record in &records {
        assert_eq!(record.change_ref, Some(CURRENT));
        assert_eq!(record.outcome, AuditOutcome::Success);
        assert_eq!(record.actor_classification, ActorClassification::Public);
    }
    let late: Vec<bool> = rig
        .metrics
        .latencies
        .lock()
        .unwrap()
        .iter()
        .map(|(_, late)| *late)
        .collect();
    assert_eq!(late, [false, true]);
}

#[tokio::test]
async fn every_answer_is_counted_under_exactly_one_result() {
    // Level 1 row *Report result counted*.
    let rig = Rig::new().await;
    let one = rig.register(nth(1), None).await;
    rig.register(nth(2), None).await;
    rig.change(EXPECTED, CURRENT, &[&one]).await;
    let answers = [
        rig.report_as(nth(1), response(EARLIER, "failed", None))
            .await,
        rig.report_as(nth(1), response(CURRENT, "success", Some(EXPECTED)))
            .await,
        rig.report_as(nth(1), response(CURRENT, "success", Some(EXPECTED)))
            .await,
        rig.report_as(nth(2), response(CURRENT, "success", Some(EXPECTED)))
            .await,
        rig.report_as(nth(3), response(CURRENT, "success", Some(EXPECTED)))
            .await,
    ]
    .map(|answer| answer.expect("answered"));
    assert_eq!(
        answers,
        [
            ReportResult::Ignored,
            ReportResult::Applied,
            ReportResult::Ignored,
            ReportResult::Ignored,
            ReportResult::NotSubscribed,
        ]
    );
    assert_eq!(
        rig.reports(),
        [
            "ignored_stale",
            "applied",
            "ignored_repeat",
            "ignored_no_row",
            "not_subscribed"
        ]
    );
}

#[tokio::test]
async fn the_response_latency_runs_from_the_states_changed_at() {
    // Neither a delivery stamp, a call of silence nor a re-arm moves the
    // baseline: the latency spans from the change that set the expectation.
    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    rig.backdate(ONE_INSTANCE, time::Duration::hours(1)).await;
    rig.silence(ONE_INSTANCE, CURRENT).await;
    // A stale report re-arms the state, touching `updated_at` only.
    rig.report(response(EARLIER, "failed", None))
        .await
        .expect("re-armed");
    let state = rig.state().await.unwrap();
    assert_eq!(state.status, ActivationStatus::Awaiting);
    assert!(state.updated_at - state.changed_at >= time::Duration::minutes(59));

    rig.report(response(CURRENT, "success", Some(EXPECTED)))
        .await
        .expect("settled");

    let latencies = rig.metrics.latencies.lock().unwrap().clone();
    let [(waited, late)] = latencies.as_slice() else {
        panic!("one observation, got {latencies:?}");
    };
    assert!(
        *waited >= std::time::Duration::from_mins(59)
            && *waited < std::time::Duration::from_hours(2),
        "from changed_at, not updated_at: {waited:?}"
    );
    assert!(!late, "the re-armed state was awaiting when settled");
}

// ---------------------------------------------------------------------------
// The report's span
// ---------------------------------------------------------------------------

#[tokio::test]
async fn each_report_runs_in_an_activation_report_span_with_its_outcome_and_result_only() {
    use tracing::Instrument as _;

    use crate::domain::activation::testing::SpanLog;

    let rig = Rig::new().await;
    rig.seed(EXPECTED, CURRENT).await;
    let (spans, _guard) = SpanLog::start();
    let caller = tracing::info_span!("caller");
    for (instance_id, response) in [
        (ONE_INSTANCE, response(CURRENT, "success", Some(EXPECTED))),
        (ONE_INSTANCE, response(CURRENT, "failed", None)),
        (nth(9), response(CURRENT, "success", Some(EXPECTED))),
    ] {
        rig.report_as(instance_id, response)
            .instrument(caller.clone())
            .await
            .expect("answered");
    }
    let caller = spans.named("caller").remove(0);
    let reported = spans.named("activation.report");
    let sensitive = [
        KEY.to_owned(),
        EXPECTED.to_owned(),
        CURRENT.to_string(),
        ONE_INSTANCE.to_string(),
        nth(9).to_string(),
        GEAR.to_string(),
    ];
    let fields: Vec<Vec<(String, String)>> = reported
        .into_iter()
        .map(|span| {
            assert_eq!(
                span.parent,
                Some(caller.id),
                "the Recorder's transaction runs in a child of the caller's span"
            );
            for (name, value) in &span.fields {
                assert!(
                    sensitive.iter().all(|s| !value.contains(s.as_str())),
                    "`{name}` carries no key, value, change_ref, instance or subscriber"
                );
            }
            span.fields
        })
        .collect();
    let named = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect()
    };
    assert_eq!(
        fields,
        [
            named(&[
                ("outcome", "success"),
                ("result", "applied"),
                ("status", "succeeded"),
            ]),
            named(&[("outcome", "failed"), ("result", "ignored_repeat")]),
            named(&[("outcome", "success"), ("result", "not_subscribed")]),
        ]
    );
}
